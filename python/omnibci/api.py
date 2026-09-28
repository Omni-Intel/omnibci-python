from __future__ import annotations

import json
import math
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Iterator, Literal

import numpy as np
from numpy.typing import NDArray

from . import _native

_MODES = ("both_bias", "eeg", "no_bias", "shorted", "test")


def _timeout(value: float) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (float, int))
        or not math.isfinite(value)
        or not 0 < value <= 60
    ):
        raise ValueError("timeout must be finite and within (0, 60] seconds")
    return float(value)


@dataclass(frozen=True)
class DeviceInfo:
    id: str
    name: str
    transport: Literal["serial", "ble"]
    address: str
    rssi_dbm: int | None = None

    @property
    def endpoint(self) -> str:
        return f"{self.transport}://{self.id}"


@dataclass(frozen=True)
class DeviceMetadata:
    firmware: str
    hardware: str
    protocol: str


@dataclass(frozen=True)
class FrontendConfig:
    reference: Literal["srb1", "srb2"] = "srb1"
    mode: Literal["both_bias", "eeg", "no_bias", "shorted", "test"] = "eeg"
    enabled_mask: int = 255
    bias_mask: int = 255
    srb2_mask: int = 0
    gains: tuple[int, ...] = (24,) * 8
    verified: bool = False

    def __post_init__(self) -> None:
        if self.reference not in ("srb1", "srb2") or self.mode not in _MODES:
            raise ValueError("unsupported reference or mode")
        for name in ("enabled_mask", "bias_mask", "srb2_mask"):
            value = getattr(self, name)
            if type(value) is not int or not 0 <= value <= 255:
                raise ValueError(f"{name} must be an integer in [0, 255]")
        gains = tuple(self.gains)
        if len(gains) != 8 or any(
            type(g) is not int or g not in (1, 2, 4, 6, 8, 12, 24) for g in gains
        ):
            raise ValueError("gains must contain 8 values from 1, 2, 4, 6, 8, 12, 24")
        if self.bias_mask & ~self.enabled_mask or self.srb2_mask & ~self.enabled_mask:
            raise ValueError("BIAS and SRB2 channels must be enabled")
        if self.reference == "srb1" and self.srb2_mask:
            raise ValueError("SRB1 configuration cannot enable SRB2 channels")
        object.__setattr__(self, "gains", gains)

    def _wire(self) -> dict:
        return dict(
            reference=int(self.reference == "srb2"),
            mode=_MODES.index(self.mode),
            enabled_mask=self.enabled_mask,
            bias_mask=self.bias_mask,
            srb2_mask=self.srb2_mask,
            gains=self.gains,
        )

    @classmethod
    def _from_wire(cls, value: dict) -> FrontendConfig:
        return cls(
            reference=("srb1", "srb2")[value["reference"]],
            mode=_MODES[value["mode"]],
            enabled_mask=value["enabled_mask"],
            bias_mask=value["bias_mask"],
            srb2_mask=value["srb2_mask"],
            gains=tuple(value["gains"]),
            verified=value["verified"],
        )


@dataclass(frozen=True, eq=False)
class SampleBatch:
    """Read-only arrays in sample-major order; gaps are preserved in sample_indices."""

    eeg_uv: NDArray[np.float32]
    raw_counts: NDArray[np.int32]
    sequence: NDArray[np.uint32]
    valid: NDArray[np.bool_]
    sample_indices: NDArray[np.uint64]
    status: NDArray[np.uint8]
    mode: NDArray[np.uint8]
    generation: int
    sample_rate_hz: int
    received_at: float

    def __len__(self) -> int:
        return len(self.sequence)

    @property
    def sample_time_s(self) -> NDArray[np.float64]:
        return self.sample_indices.astype(np.float64) / self.sample_rate_hz

    def _slice(self, start: int, stop: int | None = None) -> SampleBatch:
        return SampleBatch(
            *(getattr(self, name)[start:stop] for name in _ARRAY_FIELDS),
            self.generation,
            self.sample_rate_hz,
            self.received_at,
        )

    @classmethod
    def _from_wire(cls, data: dict) -> SampleBatch:
        arrays = [
            np.frombuffer(data[name], dtype=dtype) for name, dtype in _ARRAY_DTYPES
        ]
        n = len(arrays[2])
        arrays[0] = arrays[0].reshape(n, 8)
        arrays[1] = arrays[1].reshape(n, 8)
        arrays[5] = arrays[5].reshape(n, 3)
        if any(len(a) != n for a in arrays):
            raise _native.TransportError("inconsistent sample buffer lengths")
        return cls(
            *arrays,
            data["generation"],
            data["sample_rate_hz"],
            time.monotonic() - data["received_age_s"],
        )


_ARRAY_DTYPES = (
    ("eeg_uv", "<f4"),
    ("raw_counts", "<i4"),
    ("sequence", "<u4"),
    ("valid", "?"),
    ("sample_indices", "<u8"),
    ("status", "u1"),
    ("mode", "u1"),
)
_ARRAY_FIELDS = tuple(name for name, _ in _ARRAY_DTYPES)


def _combine(batches: list[SampleBatch]) -> SampleBatch | None:
    if not batches:
        return None
    if len(batches) == 1:
        return batches[0]
    first = batches[0]
    if any(
        (b.generation, b.sample_rate_hz) != (first.generation, first.sample_rate_hz)
        for b in batches
    ):
        raise _native.TransportError(
            "cannot concatenate different acquisition generations"
        )
    arrays = []
    for name in _ARRAY_FIELDS:
        joined = np.concatenate([getattr(b, name) for b in batches])
        # Byte-backed arrays preserve ownership and read-only semantics after native objects are freed.
        arrays.append(
            np.frombuffer(joined.tobytes(), dtype=joined.dtype).reshape(joined.shape)
        )
    return SampleBatch(
        *arrays, first.generation, first.sample_rate_hz, batches[-1].received_at
    )


@dataclass(frozen=True)
class Snapshot:
    state: str
    metadata: DeviceMetadata | None
    config: FrontendConfig | None
    generation: int
    samples: int
    missing_samples: int
    crc_errors: int
    ble: dict[str, int]
    error: str | None


class PartialReadError(_native.DeviceTimeoutError):
    """A fixed-size read timed out; partial contains the samples already consumed."""

    def __init__(self, partial: SampleBatch | None):
        self.partial = partial
        super().__init__(
            f"read timed out after receiving {len(partial) if partial is not None else 0} samples"
        )


class Board:
    """One device owner. Use connect() and a with block for explicit cleanup.

    Calls on one Board are serialized. Native hardware waits release the GIL.
    No Python callback runs in the device's ACK or receive thread.
    """

    def __init__(self, native_device):
        self._device = native_device
        self._pending: deque[SampleBatch] = deque()
        self._lock = threading.RLock()
        self._closed = False

    @classmethod
    def connect(cls, device: str | DeviceInfo, *, timeout: float = 15.0) -> Board:
        endpoint = device.endpoint if isinstance(device, DeviceInfo) else device
        if not isinstance(endpoint, str):
            raise TypeError("device must be an endpoint string or DeviceInfo")
        return cls(_native.NativeDevice.connect(endpoint, _timeout(timeout)))

    @staticmethod
    def discover(
        *, transport: Literal["serial", "ble"] = "serial", timeout: float = 10.0
    ) -> list[DeviceInfo]:
        timeout = _timeout(timeout)
        if transport == "serial":
            # Enumeration does not open ports; entries are candidates, not verified OmniBCI devices.
            return [
                DeviceInfo(port, port, "serial", port)
                for port in _native.serial_ports()
            ]
        if transport == "ble":
            return [
                DeviceInfo(transport="ble", **item)
                for item in json.loads(_native.discover_ble_json(timeout))
            ]
        raise ValueError("transport must be serial or ble")

    @property
    def snapshot(self) -> Snapshot:
        with self._lock:
            value = json.loads(self._device.snapshot_json())
            value["metadata"] = (
                DeviceMetadata(**value["metadata"]) if value["metadata"] else None
            )
            value["config"] = (
                FrontendConfig._from_wire(value["config"]) if value["config"] else None
            )
            return Snapshot(**value)

    @property
    def info(self) -> DeviceMetadata | None:
        return self.snapshot.metadata

    def get_config(self) -> FrontendConfig:
        """Return the SDK's last confirmed hardware configuration."""
        config = self.snapshot.config
        if config is None:
            raise _native.InvalidStateError("configuration has not been confirmed")
        return config

    def _ensure_open(self) -> None:
        if self._closed:
            raise _native.DisconnectedError("board is closed")

    def configure(self, config: FrontendConfig, *, timeout: float = 3.0) -> None:
        if not isinstance(config, FrontendConfig):
            raise TypeError("config must be FrontendConfig")
        with self._lock:
            self._ensure_open()
            self._device.configure(json.dumps(config._wire()), _timeout(timeout))

    def start(self, *, timeout: float = 5.0) -> None:
        with self._lock:
            self._ensure_open()
            self._device.start(_timeout(timeout))
            self._pending.clear()

    def stop(self, *, timeout: float = 5.0) -> None:
        with self._lock:
            self._ensure_open()
            self._device.stop(_timeout(timeout))

    def read(self, samples: int | None = None, *, timeout: float = 2.0) -> SampleBatch:
        """Read received samples (not padded time positions). None returns one available batch.

        Fixed-size reads retain surplus samples for the next call. On timeout,
        PartialReadError.partial contains consumed samples and may be None.
        Sequence gaps remain visible in sample_indices; no zero filling is applied.
        """
        timeout = _timeout(timeout)
        if samples is not None and (
            type(samples) is not int or not 1 <= samples <= 1_000_000
        ):
            raise ValueError("samples must be None or an integer in [1, 1000000]")
        with self._lock:
            self._ensure_open()
            deadline = time.monotonic() + timeout
            parts: list[SampleBatch] = []
            count = 0
            while True:
                if self._pending:
                    batch = self._pending.popleft()
                else:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise PartialReadError(_combine(parts))
                    try:
                        batch = SampleBatch._from_wire(self._device.read(remaining))
                    except _native.DeviceTimeoutError as error:
                        raise PartialReadError(_combine(parts)) from error
                    except _native.OmniBCIError as error:
                        error.partial = _combine(parts)
                        raise
                if samples is None:
                    return batch
                take = min(samples - count, len(batch))
                parts.append(batch._slice(0, take))
                if take < len(batch):
                    self._pending.appendleft(batch._slice(take))
                count += take
                if count == samples:
                    combined = _combine(parts)
                    assert combined is not None
                    return combined

    def iter_batches(
        self, samples: int | None = None, *, timeout: float = 2.0
    ) -> Iterator[SampleBatch]:
        while True:
            yield self.read(samples, timeout=timeout)

    def close(self, *, timeout: float = 6.0) -> None:
        with self._lock:
            if self._closed:
                return
            self._device.close(_timeout(timeout))
            self._closed = True
            self._pending.clear()

    def __enter__(self) -> Board:
        self._ensure_open()
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        try:
            self.close()
        except Exception as close_error:
            if exc is None:
                raise
            exc.add_note(f"OmniBCI cleanup also failed: {close_error}")
        return False


def decode_frames(data: bytes, *, gains: tuple[int, ...] = (24,) * 8) -> SampleBatch:
    """Decode a complete raw serial buffer using the SDK, without touching hardware.

    Not a streaming parser: trailing partial frames are not retained between calls.
    CRC-invalid frames are skipped by the SDK. Suitable for complete captured buffers.
    """
    config = FrontendConfig(gains=gains)
    if not isinstance(data, bytes):
        raise TypeError("data must be bytes")
    return SampleBatch._from_wire(_native.decode_frames(data, config.gains))
