import binascii
import gc
import json
from collections import deque

import numpy as np
import omnibci as o
import pytest


def frame(seq=0, values=(0, 1, -1, 8388607, -8388608, 42, -42, 1234), valid=True):
    data = bytearray(48)
    data[:4] = bytes([0xA5, 0x5A, 1, 1])
    data[4:8] = seq.to_bytes(4, "little")
    data[12:15] = b"\xc0\x00\x01"
    data[15] = 3 if valid else 0
    for channel, value in enumerate(values):
        data[16 + 3 * channel : 19 + 3 * channel] = (value & 0xFFFFFF).to_bytes(
            3, "big"
        )
    data[43] = 1
    data[46:] = binascii.crc_hqx(data[:46], 0xFFFF).to_bytes(2, "little")
    return bytes(data)


def wire(sequences):
    return o._native.decode_frames(b"".join(frame(s) for s in sequences), [24] * 8)


def test_native_decode_signed_values_scaling_and_ownership():
    gains = (1, 2, 4, 6, 8, 12, 24, 24)
    batch = o.decode_frames(frame(17), gains=gains)
    counts = np.array([0, 1, -1, 8388607, -8388608, 42, -42, 1234])
    assert batch.eeg_uv.shape == (1, 8)
    np.testing.assert_array_equal(batch.raw_counts[0], counts)
    np.testing.assert_allclose(
        batch.eeg_uv[0], counts * 4.5e6 / (8388607 * np.array(gains)), rtol=2e-7
    )
    assert batch.sequence.tolist() == [17]
    assert batch.status.tolist() == [[192, 0, 1]]
    assert batch.valid.tolist() == [True]
    assert batch.sample_rate_hz == 250
    retained = batch.raw_counts
    del batch
    gc.collect()
    np.testing.assert_array_equal(retained[0], counts)
    with pytest.raises(ValueError):
        retained[0, 0] = 12
    with pytest.raises(ValueError):
        retained.setflags(write=True)


def test_crc_recovery_gaps_and_rollover():
    bad = bytearray(frame(1))
    bad[20] ^= 1
    batch = o.decode_frames(
        b"noise" + frame(0xFFFFFFFF) + frame(0) + bytes(bad) + frame(3, valid=False)
    )
    assert batch.sequence.tolist() == [0xFFFFFFFF, 0, 3]
    assert batch.sample_indices.tolist() == [0, 1, 4]
    assert batch.valid.tolist() == [True, True, False]
    np.testing.assert_allclose(batch.sample_time_s, [0, 0.004, 0.016])


def test_empty_and_truncated_input():
    for data in (b"", b"garbage", frame()[:47]):
        batch = o.decode_frames(data)
        assert batch.eeg_uv.shape == (0, 8)
        assert len(batch) == 0


@pytest.mark.parametrize(
    "kwargs",
    [
        dict(gains=(24,) * 7),
        dict(gains=(3,) * 8),
        dict(gains=(True,) * 8),
        dict(enabled_mask=256),
        dict(enabled_mask=1),
        dict(srb2_mask=1),
        dict(reference="unknown"),
        dict(mode="unknown"),
    ],
)
def test_config_rejects_invalid_values(kwargs):
    with pytest.raises(ValueError):
        o.FrontendConfig(**kwargs)


@pytest.mark.parametrize("timeout", [0, -1, float("nan"), float("inf"), 61, True])
def test_timeout_validation_before_hardware(timeout):
    with pytest.raises(ValueError):
        o.Board.connect("serial://unused", timeout=timeout)


@pytest.mark.parametrize("endpoint", ["COM3", "serial://", "ble://", "tcp://host"])
def test_native_endpoint_validation(endpoint):
    with pytest.raises(ValueError):
        o.Board.connect(endpoint)


class FakeDevice:
    def __init__(self, *events):
        self.events = deque(events)
        self.calls = []
        self.close_error = None

    def read(self, timeout):
        self.calls.append(("read", timeout))
        event = (
            self.events.popleft() if self.events else o.DeviceTimeoutError("timeout")
        )
        if isinstance(event, Exception):
            raise event
        return event

    def start(self, timeout):
        self.calls.append(("start", timeout))

    def stop(self, timeout):
        self.calls.append(("stop", timeout))

    def close(self, timeout):
        self.calls.append(("close", timeout))
        if self.close_error:
            raise self.close_error

    def configure(self, config, timeout):
        self.calls.append(("configure", json.loads(config)))

    def snapshot_json(self):
        return json.dumps(
            dict(
                state="ready",
                metadata=dict(firmware="1", hardware="ADS1299", protocol="1"),
                config=dict(o.FrontendConfig()._wire(), verified=True),
                generation=1,
                samples=0,
                missing_samples=0,
                crc_errors=0,
                ble={},
                error=None,
            )
        )


def test_exact_read_keeps_surplus_and_arrays_immutable():
    fake = FakeDevice(wire([1, 2]), wire([3, 4, 5]))
    board = o.Board(fake)
    first = board.read(3)
    assert first.sequence.tolist() == [1, 2, 3]
    assert board.read(2).sequence.tolist() == [4, 5]
    assert len(fake.calls) == 2
    with pytest.raises(ValueError):
        first.eeg_uv.setflags(write=True)


def test_timeout_returns_consumed_partial_without_replay():
    board = o.Board(
        FakeDevice(wire([1, 2]), o.DeviceTimeoutError("timeout"), wire([3]))
    )
    with pytest.raises(o.PartialReadError) as error:
        board.read(3)
    assert error.value.partial.sequence.tolist() == [1, 2]
    assert board.read().sequence.tolist() == [3]
    with pytest.raises(o.PartialReadError) as error:
        board.read()
    assert error.value.partial is None


def test_transport_error_preserves_partial():
    board = o.Board(FakeDevice(wire([1]), o.BufferOverflowError("full")))
    with pytest.raises(o.BufferOverflowError) as error:
        board.read(2)
    assert error.value.partial.sequence.tolist() == [1]


def test_restart_drops_pending_stop_preserves_pending():
    fake = FakeDevice(wire([1, 2, 3]), wire([100]))
    board = o.Board(fake)
    board.read(1)
    board.stop()
    assert board.read(1).sequence.tolist() == [2]
    board.start()
    assert board.read().sequence.tolist() == [100]


def test_context_close_idempotent_and_operations_rejected():
    fake = FakeDevice()
    with o.Board(fake) as board:
        assert board.info.hardware == "ADS1299"
        assert board.get_config().verified
        board.configure(o.FrontendConfig())
    board.close()
    assert sum(call[0] == "close" for call in fake.calls) == 1
    with pytest.raises(o.DisconnectedError):
        board.read()
    with pytest.raises(o.DisconnectedError):
        board.start()


def test_cleanup_does_not_hide_original_exception():
    fake = FakeDevice()
    fake.close_error = o.TransportError("close failed")
    with pytest.raises(ValueError, match="original") as error:
        with o.Board(fake):
            raise ValueError("original")
    assert "close failed" in error.value.__notes__[0]


def test_close_error_propagates_without_original_exception():
    fake = FakeDevice()
    fake.close_error = o.TransportError("close failed")
    with pytest.raises(o.TransportError):
        with o.Board(fake):
            pass


def test_discovery_mapping(monkeypatch):
    monkeypatch.setattr(o._native, "serial_ports", lambda: ["COM4"])
    assert o.Board.discover()[0].endpoint == "serial://COM4"
    monkeypatch.setattr(
        o._native,
        "discover_ble_json",
        lambda _: json.dumps(
            [dict(id="opaque-key", name="OmniBCI", address="address", rssi_dbm=-40)]
        ),
    )
    assert o.Board.discover(transport="ble")[0].endpoint == "ble://opaque-key"


def test_generation_mixing_rejected():
    first, second = wire([1]), wire([2])
    second["generation"] = 2
    with pytest.raises(o.TransportError, match="generations"):
        o.Board(FakeDevice(first, second)).read(2)


def test_nonexistent_port_maps_native_transport_failure():
    import sys

    port = (
        "COM9876" if sys.platform == "win32" else "/dev/omnibci-nonexistent-test-port"
    )
    with pytest.raises(o.TransportError):
        o.Board.connect("serial://" + port, timeout=1)


def test_sdk_revision_matches_checked_out_source():
    import pathlib
    import subprocess

    root = pathlib.Path(__file__).resolve().parents[1]
    if not (root / "sdk" / ".git").exists():
        pytest.skip("source distribution has no Git metadata")
    revision = subprocess.check_output(
        ["git", "-C", str(root / "sdk"), "rev-parse", "HEAD"], text=True
    ).strip()
    assert o.SDK_REVISION == revision
