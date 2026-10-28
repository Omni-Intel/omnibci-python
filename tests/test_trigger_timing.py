import binascii
from pathlib import Path

import numpy as np
from omnibci import decode_frames, decode_triggers
from omnibci.api import _combine
from test_api import frame


def test_esp32_source_timestamps_preserve_wrap_zero_and_batch_operations():
    timestamps = [2**32 - 1, 0, 4000]
    packets = []
    for sequence, timestamp in enumerate(timestamps):
        packet = bytearray(frame(sequence))
        packet[8:12] = timestamp.to_bytes(4, "little")
        packet[46:] = binascii.crc_hqx(packet[:46], 0xFFFF).to_bytes(2, "little")
        packets.append(bytes(packet))
    batch = decode_frames(b"".join(packets))
    assert batch.source_timestamp_us.dtype == np.uint32
    assert batch.source_timestamp_us.tolist() == timestamps
    assert batch.source_timestamp_valid.tolist() == [True, True, True]
    assert not batch.source_timestamp_us.flags.writeable
    joined = _combine([batch._slice(0, 1), batch._slice(1, None)])
    np.testing.assert_array_equal(joined.source_timestamp_us, timestamps)
    assert joined.source_timestamp_valid.all()


def test_exact_trigger_timestamps_and_multiple_events_per_anchor():
    raw = (Path(__file__).parent / "fixtures/stm32-trigger-timing.bin").read_bytes()
    events = decode_triggers(raw)
    assert [e.code for e in events] == [0, 7, 7, 255]
    assert all(e.sequence == 0 for e in events)
    for event, offset in zip(events, (1234, 1000, 500, 1)):
        assert event.timing.trigger_timestamp_us == 2**53 + 2**32 + 2000 - offset
        assert event.timing.anchor_timestamp_us == 2000
    batch = decode_frames(raw, device_type="stm32")
    assert batch.eeg_uv.shape == (2, 32)
    assert batch.source_timestamp_us.tolist() == [2**32 - 2000, 2000]
    assert batch.source_timestamp_valid.all()
    assert not batch.source_timestamp_us.flags.writeable
    joined = _combine([batch._slice(0, 1), batch._slice(1, None)])
    np.testing.assert_array_equal(joined.source_timestamp_us, batch.source_timestamp_us)
    np.testing.assert_array_equal(
        joined.source_timestamp_valid, batch.source_timestamp_valid
    )
    assert decode_triggers(raw[:-1]) == events[:-1]


def test_corrupt_event_crc_is_not_delivered():
    raw = bytearray(
        (Path(__file__).parent / "fixtures/stm32-trigger-timing.bin").read_bytes()
    )
    raw[-1] ^= 1
    assert len(decode_triggers(bytes(raw))) == 3
