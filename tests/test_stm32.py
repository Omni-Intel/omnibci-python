import binascii

import numpy as np
import omnibci as o
import pytest


def frame(sequence=0):
    data = bytearray(128)
    data[:4] = b"\xa5\x5a\x03\x01"
    data[4:8] = sequence.to_bytes(4, "little")
    data[12:24] = b"\xc0\x00\x01" * 4
    data[24] = 3
    values = [0, 1, -1, 8388607, -8388608, 42, -42, 1234] * 4
    for i, value in enumerate(values):
        data[25 + 3 * i : 28 + 3 * i] = (value & 0xFFFFFF).to_bytes(3, "big")
    data[125] = 4
    data[126:] = binascii.crc_hqx(data[:126], 0xFFFF).to_bytes(2, "little")
    return bytes(data)


def test_stm32_decode():
    gains = tuple(([1, 2, 4, 6, 8, 12, 24] * 5)[:32])
    batch = o.decode_frames(frame(), gains=gains, device_type="stm32")
    assert batch.eeg_uv.shape == (1, 32)
    assert batch.status.tolist() == [[192, 0, 1] * 4]
    counts = [0, 1, -1, 8388607, -8388608, 42, -42, 1234] * 4
    np.testing.assert_array_equal(batch.raw_counts[0], counts)
    np.testing.assert_allclose(
        batch.eeg_uv[0],
        np.array(counts) * 4.5e6 / (8388607 * np.array(gains)),
        rtol=2e-7,
    )
    assert not batch.eeg_uv.flags.writeable
    assert batch.sample_rate_hz == 250


def test_stm32_crc_truncation_and_wrap():
    bad = bytearray(frame(1))
    bad[50] ^= 1
    batch = o.decode_frames(
        frame(0xFFFFFFFF) + frame(0) + bad + frame(3), device_type="stm32"
    )
    assert batch.sample_indices.tolist() == [0, 1, 4]
    for data in (b"", frame()[:-1], bytes(bad)):
        assert o.decode_frames(data, device_type="stm32").eeg_uv.shape == (0, 32)


@pytest.mark.parametrize(
    "values",
    [
        dict(gains=(24,) * 8),
        dict(gains=(3,) * 32),
        dict(enabled_mask=True),
        dict(enabled_mask=2**32),
        dict(enabled_mask=-1),
        dict(enabled_mask=1, bias_mask=2),
        dict(enabled_mask=1, bias_mask=1, loff_mask=2),
        dict(mode="test", loff_mask=1),
    ],
)
def test_stm32_config_validation(values):
    with pytest.raises(ValueError):
        o.Stm32Config(**values)


def test_stm32_defaults():
    config = o.Stm32Config()
    assert config.mode == "both_bias"
    assert config.loff_mask == 0
    assert config.gains == (24,) * 32


def test_discovery_type_is_retained_and_conflicts_rejected(monkeypatch):
    import json

    calls = []
    monkeypatch.setattr(
        o._native,
        "discover_ble_json",
        lambda timeout, kind: json.dumps(
            [dict(id="key", name="OmniBCI32-BLE", address="address", rssi_dbm=-40)]
        ),
    )
    info = o.Board.discover(transport="ble", device_type="stm32")[0]
    assert info.device_type == "stm32"

    class Native:
        @staticmethod
        def connect(*args):
            calls.append(args)
            return object()

    monkeypatch.setattr(o._native, "NativeDevice", Native)
    o.Board.connect(info)
    assert calls == [("ble://key", 15.0, "stm32")]
    with pytest.raises(ValueError, match="conflicts"):
        o.Board.connect(info, device_type="esp32")
    monkeypatch.setattr(o._native, "serial_ports", lambda: ["COM1"])
    assert o.Board.discover(device_type="stm32")[0].device_type == "stm32"


def test_stm32_snapshot_and_config_type():
    import json

    from test_api import FakeDevice

    native = FakeDevice()
    snapshot = json.loads(native.snapshot_json())
    snapshot["config"] = dict(o.Stm32Config(gains=(12,) * 32)._wire(), verified=True)
    native.snapshot_json = lambda: json.dumps(snapshot)
    board = o.Board(native, "stm32")
    assert board.get_config() == o.Stm32Config(gains=(12,) * 32, verified=True)
    with pytest.raises(TypeError):
        board.configure(o.FrontendConfig())
    board.configure(board.get_config())
    assert native.calls[-1][1]["gains"] == [12] * 32
    with pytest.raises(TypeError):
        o.Board(FakeDevice()).configure(o.Stm32Config())


def test_stm32_fixed_read_partial_and_immutable_combination():
    from test_api import FakeDevice

    def packed(seq):
        return o._native.decode_frames(frame(seq), [24] * 32, "stm32")

    board = o.Board(
        FakeDevice(packed(1), packed(2), o.BufferOverflowError("full")), "stm32"
    )
    with pytest.raises(o.BufferOverflowError) as error:
        board.read(3)
    assert error.value.partial.eeg_uv.shape == (2, 32)
    assert not error.value.partial.raw_counts.flags.writeable


@pytest.mark.parametrize("device_type", ["unknown", "", None, 32])
def test_bad_device_type_rejected_before_hardware(device_type):
    with pytest.raises(ValueError):
        o.decode_frames(b"", device_type=device_type)
    with pytest.raises(ValueError):
        o.Board.discover(device_type=device_type)


def test_packed_shape_rejected():
    data = o._native.decode_frames(frame(), [24] * 32, "stm32")
    data["channel_count"] = 8
    with pytest.raises(o.TransportError):
        o.SampleBatch._from_wire(data)
    data = o._native.decode_frames(frame(), [24] * 32, "stm32")
    data["eeg_uv"] = b"\x00"
    with pytest.raises(o.TransportError):
        o.SampleBatch._from_wire(data)


def test_malformed_native_batch_keeps_consumed_partial():
    from test_api import FakeDevice

    first = o._native.decode_frames(frame(1), [24] * 32, "stm32")
    malformed = o._native.decode_frames(frame(2), [24] * 32, "stm32")
    malformed["raw_counts"] = b"\x00"
    board = o.Board(FakeDevice(first, malformed), "stm32")
    with pytest.raises(o.TransportError) as error:
        board.read(2)
    assert error.value.partial.sequence.tolist() == [1]
