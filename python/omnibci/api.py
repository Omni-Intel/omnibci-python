from __future__ import annotations

import json
import math
import threading
import time
from collections import deque
from dataclasses import dataclass, replace
from typing import Iterator, Literal

import numpy as np
from numpy.typing import NDArray

from . import _native, impedance

_MODES = ("both_bias", "eeg", "no_bias", "shorted", "test")
_IMPEDANCE_MODES = ("both_bias", "eeg", "no_bias")
# The GUI clears its impedance ring after this delivery outage.
_IMPEDANCE_DATA_TIMEOUT_S = 2.0
_LEAD_OFF_TIMEOUT_S = 3.0


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
    device_type: Literal["esp32", "stm32"] = "esp32"

    @property
    def endpoint(self) -> str:
        return f"{self.transport}://{self.id}"


@dataclass(frozen=True)
class ComponentFirmware:
    """Independently reported firmware provenance for one device component.

    ``state`` is one of ``available``, ``unsupported``, ``timeout`` or
    ``read_failed``. Version, revision and dirty are absent when the component
    cannot report trustworthy provenance.
    """

    version: str | None = None
    revision: str | None = None
    dirty: bool | None = None
    state: str = ""
    protocol: str = ""

    @classmethod
    def _from_wire(cls, value: dict) -> ComponentFirmware:
        return cls(
            version=value.get("version"),
            revision=value.get("revision"),
            dirty=value.get("dirty"),
            state=value.get("state", ""),
            protocol=value.get("protocol", ""),
        )


@dataclass(frozen=True)
class DeviceMetadata:
    firmware: str
    hardware: str
    protocol: str
    stm32: ComponentFirmware | None = None
    e73: ComponentFirmware | None = None

    @classmethod
    def _from_wire(cls, value: dict) -> DeviceMetadata:
        # Older ESP32 snapshots and mocks omit the component fields entirely.
        return cls(
            firmware=value["firmware"],
            hardware=value["hardware"],
            protocol=value["protocol"],
            stm32=(
                ComponentFirmware._from_wire(value["stm32"])
                if value.get("stm32")
                else None
            ),
            e73=(
                ComponentFirmware._from_wire(value["e73"]) if value.get("e73") else None
            ),
        )


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
        # Firmware retains the SRB2 selection in SRB1 mode; the reference
        # mode controls whether those hardware switches are active.
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


@dataclass(frozen=True)
class Stm32Config:
    """Confirmed full configuration requires AE/BE-capable 32.0.5 firmware."""

    mode: Literal["both_bias", "eeg", "no_bias", "shorted", "test"] = "both_bias"
    enabled_mask: int = 0xFFFFFFFF
    bias_mask: int = 0xFFFFFFFF
    loff_mask: int = 0
    gains: tuple[int, ...] = (24,) * 32
    verified: bool = False

    def __post_init__(self) -> None:
        if self.mode not in _MODES:
            raise ValueError("unsupported mode")
        for name in ("enabled_mask", "bias_mask", "loff_mask"):
            value = getattr(self, name)
            if type(value) is not int or not 0 <= value <= 0xFFFFFFFF:
                raise ValueError(f"{name} must be an integer in [0, 4294967295]")
        gains = tuple(self.gains)
        if len(gains) != 32 or any(
            type(g) is not int or g not in (1, 2, 4, 6, 8, 12, 24) for g in gains
        ):
            raise ValueError("gains must contain 32 values from 1, 2, 4, 6, 8, 12, 24")
        if self.bias_mask & ~self.enabled_mask or self.loff_mask & ~self.enabled_mask:
            raise ValueError("BIAS and LOFF channels must be enabled")
        if self.loff_mask and _MODES.index(self.mode) > 2:
            raise ValueError("LOFF requires both_bias, eeg or no_bias mode")
        object.__setattr__(self, "gains", gains)

    def _wire(self) -> dict:
        return dict(
            mode=_MODES.index(self.mode),
            enabled_mask=self.enabled_mask,
            bias_mask=self.bias_mask,
            loff_mask=self.loff_mask,
            gains=self.gains,
        )

    @classmethod
    def _from_wire(cls, value: dict) -> Stm32Config:
        return cls(
            mode=_MODES[value["mode"]],
            enabled_mask=value["enabled_mask"],
            bias_mask=value["bias_mask"],
            loff_mask=value["loff_mask"],
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
    source_timestamp_us: NDArray[np.uint32] | None = None
    source_timestamp_valid: NDArray[np.bool_] | None = None

    def __post_init__(self):
        n = len(self.sequence)
        for name, dtype in (
            ("source_timestamp_us", "<u4"),
            ("source_timestamp_valid", "?"),
        ):
            value = getattr(self, name)
            if value is None:
                value = np.frombuffer(bytes(n * np.dtype(dtype).itemsize), dtype=dtype)
                object.__setattr__(self, name, value)
            if len(value) != n:
                raise ValueError("source timestamp length does not match samples")

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
            self.source_timestamp_us[start:stop],
            self.source_timestamp_valid[start:stop],
        )

    @classmethod
    def _from_wire(cls, data: dict) -> SampleBatch:
        try:
            width = data.get("channel_count", 8)
            status_width = data.get("status_width", 3)
            if (
                type(width) is not int
                or type(status_width) is not int
                or (width, status_width) not in ((8, 3), (32, 12))
            ):
                raise ValueError("unsupported sample shape")
            arrays = [
                np.frombuffer(data[name], dtype=dtype) for name, dtype in _ARRAY_DTYPES
            ]
            n = len(arrays[2])
            arrays[0] = arrays[0].reshape(n, width)
            arrays[1] = arrays[1].reshape(n, width)
            arrays[5] = arrays[5].reshape(n, status_width)
            if any(len(a) != n for a in arrays):
                raise ValueError("inconsistent sample buffer lengths")
        except (ValueError, TypeError, KeyError) as error:
            raise _native.TransportError("invalid sample buffers") from error
        return cls(
            *arrays,
            data["generation"],
            data["sample_rate_hz"],
            time.monotonic() - data["received_age_s"],
            np.frombuffer(data.get("source_timestamp_us", bytes(n * 4)), dtype="<u4"),
            np.frombuffer(data.get("source_timestamp_valid", bytes(n)), dtype="?"),
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
        *arrays,
        first.generation,
        first.sample_rate_hz,
        batches[-1].received_at,
        np.frombuffer(
            np.concatenate([b.source_timestamp_us for b in batches]).tobytes(),
            dtype="<u4",
        ),
        np.frombuffer(
            np.concatenate([b.source_timestamp_valid for b in batches]).tobytes(),
            dtype="?",
        ),
    )


@dataclass(frozen=True)
class Snapshot:
    state: str
    metadata: DeviceMetadata | None
    config: FrontendConfig | Stm32Config | None
    generation: int
    samples: int
    missing_samples: int
    crc_errors: int
    ble: dict[str, int]
    error: str | None


@dataclass(frozen=True)
class ImpedanceResult:
    """One ESP32 or STM32 lead-off measurement.

    ``values_kohm`` has 8 or 32 entries, matching the device. Channels outside ``mask``, channels
    rejected by full-scale clipping and channels without enough finite valid
    samples stay None. ``clipped_mask`` is restricted to ``mask``.
    """

    mask: int
    series_kohm: float
    values_kohm: tuple[float | None, ...]
    clipped_mask: int


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

    def __init__(self, native_device, device_type="esp32"):
        self._device = native_device
        self._device_type = device_type
        self._pending: deque[SampleBatch] = deque()
        self._lock = threading.RLock()
        self._closed = False

    @classmethod
    def connect(
        cls,
        device: str | DeviceInfo,
        *,
        timeout: float = 15.0,
        device_type: Literal["esp32", "stm32"] | None = None,
    ) -> Board:
        if isinstance(device, DeviceInfo):
            if device_type is not None and device_type != device.device_type:
                raise ValueError("device_type conflicts with discovered device")
            device_type = _device_type(device.device_type)
        else:
            device_type = _device_type("esp32" if device_type is None else device_type)
        endpoint = device.endpoint if isinstance(device, DeviceInfo) else device
        if not isinstance(endpoint, str):
            raise TypeError("device must be an endpoint string or DeviceInfo")
        timeout = _timeout(timeout)
        native = (
            _native.NativeDevice.connect(endpoint, timeout)
            if device_type == "esp32"
            else _native.NativeDevice.connect(endpoint, timeout, device_type)
        )
        return cls(native, device_type)

    @staticmethod
    def discover(
        *,
        transport: Literal["serial", "ble"] = "serial",
        timeout: float = 10.0,
        device_type: Literal["esp32", "stm32"] = "esp32",
    ) -> list[DeviceInfo]:
        timeout = _timeout(timeout)
        _device_type(device_type)
        if transport == "serial":
            # Enumeration does not open ports; entries are candidates, not verified OmniBCI devices.
            return [
                DeviceInfo(port, port, "serial", port, device_type=device_type)
                for port in _native.serial_ports()
            ]
        if transport == "ble":
            return [
                DeviceInfo(transport="ble", device_type=device_type, **item)
                for item in json.loads(
                    _native.discover_ble_json(timeout)
                    if device_type == "esp32"
                    else _native.discover_ble_json(timeout, device_type)
                )
            ]
        raise ValueError("transport must be serial or ble")

    @property
    def snapshot(self) -> Snapshot:
        with self._lock:
            value = json.loads(self._device.snapshot_json())
            value["metadata"] = (
                DeviceMetadata._from_wire(value["metadata"])
                if value["metadata"]
                else None
            )
            value["config"] = (
                (
                    Stm32Config if self._device_type == "stm32" else FrontendConfig
                )._from_wire(value["config"])
                if value["config"]
                else None
            )
            return Snapshot(**value)

    @property
    def info(self) -> DeviceMetadata | None:
        return self.snapshot.metadata

    def get_config(self) -> FrontendConfig | Stm32Config:
        """Return the SDK's last confirmed hardware configuration."""
        config = self.snapshot.config
        if config is None:
            raise _native.InvalidStateError("configuration has not been confirmed")
        return config

    def _ensure_open(self) -> None:
        if self._closed:
            raise _native.DisconnectedError("board is closed")

    def configure(
        self, config: FrontendConfig | Stm32Config, *, timeout: float = 3.0
    ) -> None:
        expected = Stm32Config if self._device_type == "stm32" else FrontendConfig
        if not isinstance(config, expected):
            raise TypeError(f"config must be {expected.__name__}")
        with self._lock:
            self._ensure_open()
            self._device.configure(json.dumps(config._wire()), _timeout(timeout))

    def start(self, *, timeout: float = 5.0) -> None:
        with self._lock:
            self._ensure_open()
            self._device.start(_timeout(timeout))
            self._pending.clear()

    def read_triggers(self) -> tuple[TriggerEvent, ...]:
        """Drain currently available STM32 events independently of EEG reads.

        Nonblocking. Call regularly; a 16384-event queue reports overflow.
        Stop preserves queued events; a successful new start clears them.
        """
        with self._lock:
            self._ensure_open()
            return _trigger_events(json.loads(self._device.read_triggers_json()))

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

    def measure_impedance(
        self,
        *,
        mask: int | None = None,
        series_kohm: float | None = None,
        window_seconds: float = 4.0,
        timeout: float = 15.0,
    ) -> ImpedanceResult:
        """Measure 8-channel ESP32 or 32-channel STM32 impedance.

        Stop acquisition, suppress BIAS feedback, enable lead-off, collect the
        requested window, then disable lead-off and restore configuration.
        Acquisition stays stopped. The window defaults to 4 seconds and accepts
        1 to 60 seconds, rounded up to whole samples; timeout is independent.
        ESP32 SRB2 defaults to 4.40 kOhm series compensation; otherwise 9.98.
        The returned tuple has one entry per physical channel.
        """
        measure = (
            self._measure_impedance_stm32
            if self._device_type == "stm32"
            else self._measure_impedance_esp32
        )
        return measure(
            mask=mask,
            series_kohm=series_kohm,
            window_seconds=window_seconds,
            timeout=timeout,
        )

    def _measure_impedance_esp32(
        self,
        *,
        mask: int | None = None,
        series_kohm: float | None = None,
        window_seconds: float = 4.0,
        timeout: float = 15.0,
    ) -> ImpedanceResult:
        """Measure ESP32 electrode impedance, then leave acquisition stopped.

        The whole transaction holds the Board lock: confirm the current
        configuration, stop streaming, disable BIAS feedback and enable 31.25 Hz
        lead-off on the selected enabled channels, read the requested sample window,
        stop, disable lead-off, and restore the original configuration. The
        board stays ready; call start() to resume EEG.

        ``window_seconds`` selects 1 to 60 seconds of sample positions
        (default 4), rounded up to a whole sample. It is not a settling delay.
        ``timeout`` is a separate collection deadline; allow enough time for
        the window and delivery overhead. Control timeouts remain separate.

        ``mask`` is an 8-bit channel mask, intersected with the enabled
        channels; it defaults to every enabled channel. ``series_kohm``
        defaults to 9.98 kOhm for SRB1 and 4.40 kOhm for SRB2. ``timeout``
        bounds collection of the window. If cleanup cannot confirm LOFF off
        or the restored configuration, the exception carries a note and the
        Board is closed.
        """
        if (
            isinstance(window_seconds, bool)
            or not isinstance(window_seconds, (int, float))
            or not math.isfinite(window_seconds)
            or not 1.0 <= window_seconds <= 60.0
        ):
            raise ValueError("window_seconds must be finite and within [1, 60] seconds")
        window_seconds = float(window_seconds)
        timeout = _timeout(timeout)
        if mask is not None and (type(mask) is not int or not 0 <= mask <= 0xFF):
            raise ValueError("mask must be an integer in [0, 255]")
        if series_kohm is not None and (
            isinstance(series_kohm, bool)
            or not isinstance(series_kohm, (int, float))
            or not math.isfinite(series_kohm)
            or not 0.0 <= series_kohm <= 1000.0
        ):
            raise ValueError("series_kohm must be finite and within [0, 1000]")

        with self._lock:
            self._ensure_open()
            original = self.get_config()
            if not isinstance(original, FrontendConfig) or not original.verified:
                raise _native.InvalidStateError(
                    "frontend configuration has not been confirmed"
                )
            if original.mode not in _IMPEDANCE_MODES:
                raise _native.ConfigurationError(
                    "impedance requires both_bias, eeg or no_bias mode"
                )
            effective_mask = (
                original.enabled_mask if mask is None else mask
            ) & original.enabled_mask
            if effective_mask == 0:
                raise ValueError("mask must select at least one enabled channel")
            if series_kohm is None:
                series_kohm = (
                    impedance.LEAD_OFF_SERIES_SRB2_KOHM
                    if original.reference == "srb2"
                    else impedance.LEAD_OFF_SERIES_SRB1_KOHM
                )
            else:
                series_kohm = float(series_kohm)
            measurement = replace(original, bias_mask=0)

            excitation_started = False
            try:
                self._stop_before_impedance()
                self._pending.clear()
                excitation_started = True
                self.configure(measurement)
                if self.get_config() != measurement:
                    raise _native.ConfigurationError(
                        "impedance measurement configuration readback does not match"
                    )
                self._set_lead_off(effective_mask)
                self.start()
                window = self._collect_impedance_window(timeout, window_seconds)
                self._stop_impedance_acquisition()
            except BaseException as primary:
                self._cleanup_impedance_esp32(original, primary, excitation_started)
                raise
            try:
                self._discard_impedance_tail()
                self._set_lead_off(0)
                self.configure(original)
                if self.get_config() != original:
                    raise _native.TransportError(
                        "restored frontend configuration readback does not match"
                    )
            except BaseException as primary:
                self._cleanup_impedance_esp32(original, primary, True)
                raise
            values, clipped_mask = impedance.estimate_impedance_kohm(
                window.eeg_uv,
                window.raw_counts,
                window.sequence,
                window.valid,
                mask=effective_mask,
                series_kohm=series_kohm,
                sample_rate_hz=window.sample_rate_hz,
            )
            return ImpedanceResult(
                mask=effective_mask,
                series_kohm=series_kohm,
                values_kohm=values,
                clipped_mask=clipped_mask,
            )

    def _measure_impedance_stm32(
        self,
        *,
        mask: int | None = None,
        series_kohm: float | None = None,
        window_seconds: float = 4.0,
        timeout: float = 15.0,
    ) -> ImpedanceResult:
        """Measure STM32 electrode impedance, then leave acquisition stopped.

        The whole transaction holds the Board lock: confirm the current
        configuration, stop streaming, enable 31.25 Hz lead-off on the selected
        enabled channels with BIAS feedback off, read the requested sample window,
        stop, and restore the original configuration with LOFF disabled. The
        board stays ready; call start() to resume EEG.

        ``window_seconds`` selects 1 to 60 seconds of sample positions
        (default 4), rounded up to a whole sample. It is not a settling delay.
        ``timeout`` is a separate collection deadline; allow enough time for
        the window and delivery overhead. Control timeouts remain separate.

        ``mask`` is a 32-bit channel mask, intersected with the enabled
        channels; it defaults to every enabled channel. ``series_kohm``
        defaults to 9.98 kOhm. ``timeout`` bounds collection of the window.
        """
        if (
            isinstance(window_seconds, bool)
            or not isinstance(window_seconds, (int, float))
            or not math.isfinite(window_seconds)
            or not 1.0 <= window_seconds <= 60.0
        ):
            raise ValueError("window_seconds must be finite and within [1, 60] seconds")
        window_seconds = float(window_seconds)
        timeout = _timeout(timeout)
        if series_kohm is None:
            series_kohm = impedance.DEFAULT_SERIES_KOHM
        elif (
            isinstance(series_kohm, bool)
            or not isinstance(series_kohm, (int, float))
            or not 0.0 <= series_kohm <= 1000.0
        ):
            raise ValueError("series_kohm must be finite and within [0, 1000]")
        else:
            series_kohm = float(series_kohm)
        if mask is not None and (type(mask) is not int or not 0 <= mask <= 0xFFFFFFFF):
            raise ValueError("mask must be an integer in [0, 4294967295]")

        with self._lock:
            self._ensure_open()
            if self._device_type != "stm32":
                raise _native.ConfigurationError(
                    "impedance measurement requires an STM32 board"
                )
            original = self.get_config()
            if not isinstance(original, Stm32Config) or not original.verified:
                raise _native.InvalidStateError(
                    "STM32 configuration has not been confirmed"
                )
            if original.mode not in _IMPEDANCE_MODES:
                raise _native.ConfigurationError(
                    "impedance requires both_bias, eeg or no_bias mode"
                )
            effective_mask = (
                original.enabled_mask if mask is None else mask
            ) & original.enabled_mask
            if effective_mask == 0:
                raise ValueError("mask must select at least one enabled channel")
            measurement = replace(original, loff_mask=effective_mask, bias_mask=0)
            restore = replace(original, loff_mask=0)

            excitation_started = False
            try:
                self._stop_before_impedance()
                self._pending.clear()
                excitation_started = True
                self.configure(measurement)
                if self.get_config() != measurement:
                    raise _native.ConfigurationError(
                        "impedance measurement configuration readback does not match"
                    )
                self.start()
                window = self._collect_impedance_window(timeout, window_seconds)
                self._stop_impedance_acquisition()
            except BaseException as primary:
                self._cleanup_impedance(restore, primary, excitation_started)
                raise
            try:
                self._discard_impedance_tail()
                self.configure(restore)
                if self.get_config() != restore:
                    raise _native.TransportError(
                        "restored STM32 configuration readback does not match"
                    )
            except BaseException as primary:
                self._cleanup_impedance(restore, primary, True)
                raise
            values, clipped_mask = impedance.estimate_impedance_kohm(
                window.eeg_uv,
                window.raw_counts,
                window.sequence,
                window.valid,
                mask=effective_mask,
                series_kohm=series_kohm,
                sample_rate_hz=window.sample_rate_hz,
            )
            return ImpedanceResult(
                mask=effective_mask,
                series_kohm=series_kohm,
                values_kohm=values,
                clipped_mask=clipped_mask,
            )

    def _stop_before_impedance(self) -> None:
        state = self.snapshot.state
        if state not in ("ready", "streaming"):
            raise _native.InvalidStateError(
                "impedance measurement requires a ready or streaming board, "
                f"got {state}"
            )
        # The GUI confirms a stop before enabling LOFF even when idle.
        self._stop_impedance_acquisition()

    def _stop_impedance_acquisition(self) -> None:
        self.stop()
        state = self.snapshot.state
        if state != "ready":
            raise _native.InvalidStateError(
                f"STM32 stop was not confirmed (state {state})"
            )

    def _collect_impedance_window(
        self, timeout: float, window_seconds: float = 4.0
    ) -> SampleBatch:
        deadline = time.monotonic() + timeout
        parts: list[SampleBatch] = []
        slots = 0
        first_index = 0
        while True:
            if parts:
                last_index = int(parts[-1].sample_indices[-1])
                if last_index - first_index + 1 >= slots:
                    break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise _native.DeviceTimeoutError(
                    "impedance window did not complete before timeout"
                )
            batch = self.read(None, timeout=remaining)
            if len(batch) == 0:
                continue
            if time.monotonic() - batch.received_at >= 2.0:
                parts.clear()
                continue
            if (
                parts
                and batch.received_at - parts[-1].received_at
                >= _IMPEDANCE_DATA_TIMEOUT_S
            ):
                parts.clear()
            if not parts:
                if batch.sample_rate_hz <= 0:
                    raise _native.TransportError(
                        "STM32 impedance window has an invalid sample rate"
                    )
                slots = math.ceil(batch.sample_rate_hz * window_seconds)
                first_index = int(batch.sample_indices[0])
            parts.append(batch)
        combined = _combine(parts)
        assert combined is not None
        # Keep the requested duration in sample positions, preserving gaps.
        start = int(combined.sample_indices[-1]) - (slots - 1)
        keep = combined.sample_indices >= start
        return SampleBatch(
            *(getattr(combined, name)[keep] for name in _ARRAY_FIELDS),
            combined.generation,
            combined.sample_rate_hz,
            combined.received_at,
            combined.source_timestamp_us[keep],
            combined.source_timestamp_valid[keep],
        )

    def _discard_impedance_tail(self) -> None:
        """Drop queued measurement samples so a later read cannot return them."""
        self._pending.clear()
        try:
            # The native queue holds at most 128 batches after confirmed STOP.
            for _ in range(129):
                self.read(None, timeout=0.001)
        except _native.DeviceTimeoutError:
            return
        raise _native.TransportError("impedance sample queue did not drain after stop")

    def _cleanup_impedance(
        self,
        restore: Stm32Config,
        primary: BaseException,
        excitation_started: bool,
    ) -> None:
        problems: list[str] = []
        restore_confirmed = False
        state = None
        try:
            state = self.snapshot.state
        except BaseException as error:
            problems.append(f"state check failed: {error}")
        if state in ("streaming", "starting", "stopping"):
            try:
                self._stop_impedance_acquisition()
                state = self.snapshot.state
            except BaseException as error:
                problems.append(f"stop failed: {error}")
        if excitation_started and state == "ready":
            try:
                self._discard_impedance_tail()
            except BaseException as error:
                problems.append(f"stale sample drain failed: {error}")
            try:
                self.configure(restore)
                if self.get_config() != restore:
                    problems.append("restored configuration readback does not match")
                else:
                    restore_confirmed = True
            except BaseException as error:
                problems.append(f"restore failed: {error}")
        elif excitation_started:
            if state == "faulted":
                # The SDK delivers queued samples before its fault. Discard
                # them, then accept the fault as the primary error.
                try:
                    self._discard_impedance_tail()
                except BaseException:
                    pass
            problems.append(
                f"device did not reach a confirmed ready state (state {state})"
            )
        self._pending.clear()
        if problems and excitation_started and not restore_confirmed:
            # Match GUI recovery: isolate uncertain hardware and ask the native
            # owner for best-effort LOFF shutdown, without claiming restoration.
            try:
                self.close()
            except BaseException as error:
                problems.append(f"close failed: {error}")
            finally:
                self._closed = True
        if problems:
            detail = "; ".join(problems)
            if excitation_started and not restore_confirmed:
                note = (
                    "impedance cleanup did not confirm LOFF disabled; "
                    f"electrode excitation may remain enabled: {detail}"
                )
            else:
                note = f"impedance cleanup did not complete: {detail}"
            primary.add_note(note)

    def _set_lead_off(self, mask: int) -> None:
        # The native call waits for a fresh 0xA9 readback matching the mask and
        # the confirmed reference, mode and enabled channels.
        self._device.set_lead_off(mask, _LEAD_OFF_TIMEOUT_S)

    def _cleanup_impedance_esp32(
        self,
        original: FrontendConfig,
        primary: BaseException,
        excitation_started: bool,
    ) -> None:
        problems: list[str] = []
        restore_confirmed = False
        loff_disabled = not excitation_started
        state = None
        try:
            state = self.snapshot.state
        except BaseException as error:
            problems.append(f"state check failed: {error}")
        if state in ("streaming", "starting", "stopping"):
            try:
                self._stop_impedance_acquisition()
                state = self.snapshot.state
            except BaseException as error:
                problems.append(f"stop failed: {error}")
        if excitation_started and state == "ready":
            try:
                self._discard_impedance_tail()
            except BaseException as error:
                problems.append(f"stale sample drain failed: {error}")
            try:
                self._set_lead_off(0)
                loff_disabled = True
            except BaseException as error:
                problems.append(f"LOFF disable failed: {error}")
            try:
                self.configure(original)
                if self.get_config() != original:
                    problems.append("restored configuration readback does not match")
                else:
                    restore_confirmed = True
            except BaseException as error:
                problems.append(f"restore failed: {error}")
        elif excitation_started:
            if state == "faulted":
                # The SDK delivers queued samples before its fault. Discard
                # them, then accept the fault as the primary error.
                try:
                    self._discard_impedance_tail()
                except BaseException:
                    pass
            problems.append(
                f"device did not reach a confirmed ready state (state {state})"
            )
        self._pending.clear()
        if (
            problems
            and excitation_started
            and (not restore_confirmed or not loff_disabled)
        ):
            # Match GUI recovery: isolate uncertain hardware and ask the native
            # owner for best-effort LOFF shutdown, without claiming restoration.
            try:
                self.close()
            except BaseException as error:
                problems.append(f"close failed: {error}")
            finally:
                self._closed = True
        if problems:
            detail = "; ".join(problems)
            if excitation_started and not loff_disabled:
                note = (
                    "impedance cleanup did not confirm LOFF disabled; "
                    f"electrode excitation may remain enabled: {detail}"
                )
            elif excitation_started and not restore_confirmed:
                note = (
                    "impedance cleanup disabled LOFF but did not confirm the "
                    f"original configuration; the session was closed: {detail}"
                )
            else:
                note = f"impedance cleanup did not complete: {detail}"
            primary.add_note(note)

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
            note = f"OmniBCI cleanup also failed: {close_error}"
            exc.add_note(note)
        return False


def decode_frames(
    data: bytes,
    *,
    gains: tuple[int, ...] | None = None,
    device_type: Literal["esp32", "stm32"] = "esp32",
) -> SampleBatch:
    """Decode a complete raw serial buffer using the SDK, without touching hardware.

    Not a streaming parser: trailing partial frames are not retained between calls.
    CRC-invalid frames are skipped by the SDK. Suitable for complete captured buffers.
    """
    _device_type(device_type)
    config_type = Stm32Config if device_type == "stm32" else FrontendConfig
    config = config_type() if gains is None else config_type(gains=gains)
    if not isinstance(data, bytes):
        raise TypeError("data must be bytes")
    packed = (
        _native.decode_frames(data, config.gains)
        if device_type == "esp32"
        else _native.decode_frames(data, config.gains, device_type)
    )
    return SampleBatch._from_wire(packed)


def _device_type(value: str) -> str:
    if value not in ("esp32", "stm32"):
        raise ValueError("device_type must be esp32 or stm32")
    return value


@dataclass(frozen=True)
class TriggerTiming:
    trigger_timestamp_us: int
    anchor_timestamp_us: int


@dataclass(frozen=True)
class TriggerEvent:
    generation: int
    code: int
    sequence: int
    source_timestamp_ms: int | None
    timing: TriggerTiming | None


def _trigger_events(value: dict) -> tuple[TriggerEvent, ...]:
    return tuple(
        TriggerEvent(
            value["generation"],
            e["code"],
            e["sequence"],
            e["source_timestamp_ms"],
            TriggerTiming(**e["timing"]) if e["timing"] is not None else None,
        )
        for e in value["events"]
    )


def decode_triggers(data: bytes) -> tuple[TriggerEvent, ...]:
    """Decode complete STM32 trigger packets offline; timestamps remain integers."""
    return _trigger_events(json.loads(_native.decode_triggers_json(data)))
