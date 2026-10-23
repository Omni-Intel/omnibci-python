import json
from collections import deque

import numpy as np
import omnibci as o
import pytest
from omnibci import impedance

RATE = 250
WINDOW = RATE * 4
TOTAL_KOHM = 22.5 + impedance.DEFAULT_SERIES_KOHM
DEFAULT_CONFIG = o.Stm32Config(
    mode="eeg",
    enabled_mask=0xFFFFFFFF,
    bias_mask=0xFFFFFFFF,
    gains=(24,) * 32,
)


def carrier(total_kohm=TOTAL_KOHM, positions=None, *, dc=100.0):
    positions = np.arange(WINDOW) if positions is None else np.asarray(positions)
    phase = 2.0 * np.pi * impedance.LEAD_OFF_FREQUENCY_HZ * positions / RATE
    return (dc + total_kohm * 24.0 / np.pi * np.sin(phase)).astype(np.float32)


def all_channels(values):
    return np.repeat(np.asarray(values, np.float32)[:, None], 32, axis=1)


def fit_window(
    values,
    *,
    counts=None,
    sequence=None,
    valid=None,
    mask=0xFFFFFFFF,
    series_kohm=impedance.DEFAULT_SERIES_KOHM,
):
    values = np.asarray(values, np.float32)
    if values.ndim == 1:
        values = all_channels(values)
    n = len(values)
    return impedance.estimate_impedance_kohm(
        values,
        np.zeros((n, 32), np.int32) if counts is None else counts,
        np.arange(n, dtype=np.uint32) if sequence is None else sequence,
        np.ones(n, bool) if valid is None else valid,
        mask=mask,
        series_kohm=series_kohm,
        sample_rate_hz=RATE,
    )


def wire(values, *, counts=None, sequence=None, valid=None, indices=None):
    values = np.asarray(values, np.float32)
    n, channels = values.shape
    assert channels == 32
    counts = np.zeros((n, 32), np.int32) if counts is None else counts
    sequence = np.arange(n, dtype=np.uint32) if sequence is None else sequence
    valid = np.ones(n, bool) if valid is None else valid
    indices = np.arange(n, dtype=np.uint64) if indices is None else indices
    return dict(
        eeg_uv=np.ascontiguousarray(values, "<f4").tobytes(),
        raw_counts=np.ascontiguousarray(counts, "<i4").tobytes(),
        sequence=np.ascontiguousarray(sequence, "<u4").tobytes(),
        valid=np.ascontiguousarray(valid, bool).tobytes(),
        sample_indices=np.ascontiguousarray(indices, "<u8").tobytes(),
        status=np.zeros((n, 12), np.uint8).tobytes(),
        mode=np.ones(n, np.uint8).tobytes(),
        generation=1,
        sample_rate_hz=RATE,
        received_age_s=0.0,
        channel_count=32,
        status_width=12,
    )


def split_batches(values, size=RATE):
    return [
        wire(
            values[start : start + size],
            sequence=np.arange(start, min(start + size, len(values))),
            indices=np.arange(start, min(start + size, len(values))),
        )
        for start in range(0, len(values), size)
    ]


class FakeDevice:
    def __init__(
        self,
        *,
        state="streaming",
        config=DEFAULT_CONFIG,
        batches=(),
        start_failures=(),
        stop_failures=(),
        configure_failures=(),
        read_error=None,
    ):
        self.state = state
        self.config = dict(config._wire(), verified=True)
        self.batches = deque(batches)
        self.start_failures = deque(start_failures)
        self.stop_failures = deque(stop_failures)
        self.configure_failures = deque(configure_failures)
        self.read_error = read_error
        self.generation = 0
        self.calls = []

    @staticmethod
    def _failure(failures):
        return failures.popleft() if failures else None

    def configure(self, config, timeout):
        parsed = json.loads(config)
        self.calls.append(("configure", parsed))
        failure = self._failure(self.configure_failures)
        if failure is not None:
            raise failure
        self.config = dict(parsed, verified=True)

    def start(self, timeout):
        self.calls.append(("start", None))
        failure = self._failure(self.start_failures)
        if failure is not None:
            raise failure
        self.state = "streaming"
        self.generation += 1

    def stop(self, timeout):
        self.calls.append(("stop", None))
        failure = self._failure(self.stop_failures)
        if failure is not None:
            self.state = "faulted"
            raise failure
        self.state = "ready"

    def read(self, timeout):
        self.calls.append(("read", timeout))
        if self.read_error is not None:
            raise self.read_error
        if self.batches:
            return self.batches.popleft()
        raise o.DeviceTimeoutError("no samples")

    def close(self, timeout):
        self.calls.append(("close", None))

    def snapshot_json(self):
        return json.dumps(
            dict(
                state=self.state,
                metadata=dict(
                    firmware="1",
                    hardware="STM32",
                    protocol="1",
                    stm32=None,
                    e73=None,
                ),
                config=self.config,
                generation=self.generation,
                samples=0,
                missing_samples=0,
                crc_errors=0,
                ble={},
                error=None,
            )
        )


def control_calls(fake):
    return [name for name, _ in fake.calls]


def configure_calls(fake):
    return [value for name, value in fake.calls if name == "configure"]


def test_fit_recovers_compensated_impedance_and_ignores_dc_offset():
    values = carrier(dc=135.0)
    result, clipped = fit_window(values)
    assert clipped == 0
    assert result[0] == pytest.approx(22.5, abs=1e-3)
    assert all(value == pytest.approx(22.5, abs=1e-3) for value in result)


def test_fit_uses_sequence_phase_across_gaps_and_wrap():
    positions = np.arange(1500)
    positions = positions[positions % 13 != 0]
    sequence = ((positions + 0xFFFFF000) % 2**32).astype(np.uint32)
    values = all_channels(carrier(positions=positions))
    result, _ = fit_window(values, sequence=sequence)
    assert result[0] == pytest.approx(22.5, abs=1e-3)


def test_fit_requires_one_second_of_finite_valid_samples():
    values = all_channels(carrier())
    valid = np.zeros(WINDOW, bool)
    valid[: RATE - 1] = True
    result, _ = fit_window(values, valid=valid)
    assert result[0] is None
    valid[RATE - 1] = True
    result, _ = fit_window(values, valid=valid)
    assert result[0] == pytest.approx(22.5, abs=1e-3)
    values[0, 0] = np.nan
    result, _ = fit_window(values, valid=valid)
    assert result[0] is None


def test_constant_window_is_rejected_while_weak_carrier_clamps_to_zero():
    result, _ = fit_window(np.full(WINDOW, 42.0))
    assert all(value is None for value in result)
    result, _ = fit_window(carrier(total_kohm=2.0))
    assert result[0] == 0.0


def test_full_scale_codes_reject_only_selected_valid_channels():
    values = all_channels(carrier())
    counts = np.zeros((WINDOW, 32), np.int32)
    counts[10, 3] = impedance.FULL_SCALE_MAX
    counts[20, 5] = impedance.FULL_SCALE_MIN
    counts[30, 7] = impedance.FULL_SCALE_MAX
    counts[40, 9] = impedance.FULL_SCALE_MAX - 1
    counts[41, 10] = impedance.FULL_SCALE_MIN + 1
    valid = np.ones(WINDOW, bool)
    valid[30] = False
    result, clipped = fit_window(values, counts=counts, valid=valid)
    assert clipped == (1 << 3) | (1 << 5)
    assert result[3] is None
    assert result[5] is None
    assert result[7] == pytest.approx(22.5, abs=1e-3)
    assert result[9] is not None
    assert result[10] is not None


def test_mask_limits_values_and_clipped_mask():
    values = all_channels(carrier())
    counts = np.zeros((WINDOW, 32), np.int32)
    counts[0, 0] = impedance.FULL_SCALE_MAX
    counts[0, 1] = impedance.FULL_SCALE_MAX
    result, clipped = fit_window(values, counts=counts, mask=0b101)
    assert clipped == 0b001
    assert result[0] is None
    assert result[2] == pytest.approx(22.5, abs=1e-3)
    assert all(value is None for index, value in enumerate(result) if index != 2)


def test_invalid_fit_arguments_are_rejected():
    values = all_channels(carrier())
    for kwargs in (
        dict(mask=-1),
        dict(mask=2**32),
        dict(mask=True),
        dict(series_kohm=-1),
        dict(series_kohm=float("nan")),
        dict(series_kohm=1001),
    ):
        with pytest.raises(ValueError):
            fit_window(values, **kwargs)
    with pytest.raises(ValueError):
        impedance.estimate_impedance_kohm(
            values,
            np.zeros_like(values, np.int32),
            np.arange(WINDOW, dtype=np.uint32),
            np.ones(WINDOW, bool),
            mask=1,
            sample_rate_hz=0,
        )


def test_measure_impedance_stops_measures_restores_and_discards_tail():
    values = all_channels(carrier(positions=np.arange(1250)))
    fake = FakeDevice(state="streaming", batches=split_batches(values))
    board = o.Board(fake, "stm32")

    result = board.measure_impedance(timeout=2.0)

    assert isinstance(result, o.ImpedanceResult)
    assert result.mask == 0xFFFFFFFF
    assert result.series_kohm == pytest.approx(9.98)
    assert result.clipped_mask == 0
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)
    calls = control_calls(fake)
    assert calls[0] == "stop"
    assert calls.count("stop") == 2
    assert calls.count("start") == 1
    configured = configure_calls(fake)
    assert configured[0]["loff_mask"] == 0xFFFFFFFF
    assert configured[0]["bias_mask"] == 0
    assert configured[0]["enabled_mask"] == 0xFFFFFFFF
    assert configured[1]["loff_mask"] == 0
    assert configured[1]["bias_mask"] == 0xFFFFFFFF
    last_configure = max(
        index for index, (name, _) in enumerate(fake.calls) if name == "configure"
    )
    assert "start" not in [name for name, _ in fake.calls[last_configure + 1 :]]
    last_stop = max(
        index for index, (name, _) in enumerate(fake.calls) if name == "stop"
    )
    assert "read" in [name for name, _ in fake.calls[last_stop + 1 :]]
    assert board.get_config().loff_mask == 0
    assert fake.state == "ready"
    assert not board._pending
    with pytest.raises(o.DeviceTimeoutError):
        board.read(timeout=0.01)


def test_measure_impedance_defaults_to_enabled_channels():
    values = all_channels(carrier())
    config = o.Stm32Config(
        mode="both_bias",
        enabled_mask=0b101,
        bias_mask=0b101,
        gains=(24,) * 32,
    )
    fake = FakeDevice(state="ready", config=config, batches=split_batches(values))
    board = o.Board(fake, "stm32")

    result = board.measure_impedance(mask=0xFF, timeout=2.0)

    assert result.mask == 0b101
    assert result.values_kohm[0] is not None
    assert result.values_kohm[1] is None
    assert result.values_kohm[2] is not None
    assert configure_calls(fake)[0]["loff_mask"] == 0b101


def test_measure_impedance_rejects_bad_arguments_and_modes():
    board = o.Board(FakeDevice(state="ready"), "stm32")
    for kwargs in (
        dict(mask=False),
        dict(mask=-1),
        dict(mask=2**32),
        dict(mask=0),
        dict(series_kohm=True),
        dict(series_kohm=-1),
        dict(series_kohm=float("inf")),
        dict(series_kohm=1001),
        dict(timeout=0),
    ):
        with pytest.raises(ValueError):
            board.measure_impedance(**kwargs)
    with pytest.raises(o.ConfigurationError):
        o.Board(FakeDevice(state="ready"), "esp32").measure_impedance()
    shorted = o.Stm32Config(
        mode="shorted", enabled_mask=1, bias_mask=0, gains=(24,) * 32
    )
    with pytest.raises(o.ConfigurationError):
        o.Board(FakeDevice(state="ready", config=shorted), "stm32").measure_impedance()


def test_configure_failure_still_restores_loff_off_without_hiding_primary():
    fake = FakeDevice(
        state="ready",
        configure_failures=[o.ConfigurationError("measurement rejected")],
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.ConfigurationError, match="measurement rejected") as error:
        board.measure_impedance(timeout=1.0)
    assert not getattr(error.value, "__notes__", [])
    assert configure_calls(fake)[-1]["loff_mask"] == 0
    assert board.get_config().loff_mask == 0
    assert fake.state == "ready"


def test_configure_and_restore_failure_reports_unconfirmed_loff():
    fake = FakeDevice(
        state="ready",
        configure_failures=[
            o.ConfigurationError("measurement rejected"),
            o.ConfigurationError("restore rejected"),
        ],
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.ConfigurationError, match="measurement rejected") as error:
        board.measure_impedance(timeout=1.0)
    note = " ".join(error.value.__notes__)
    assert "LOFF" in note
    assert "may remain enabled" in note
    assert board._closed


def test_start_failure_restores_loff_off():
    values = all_channels(carrier())
    fake = FakeDevice(
        state="ready",
        batches=split_batches(values),
        start_failures=[o.DeviceTimeoutError("start timeout")],
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.DeviceTimeoutError, match="start timeout"):
        board.measure_impedance(timeout=1.0)
    assert configure_calls(fake)[-1]["loff_mask"] == 0
    assert board.get_config().loff_mask == 0
    assert "start" in control_calls(fake)


def test_read_failure_stops_and_restores():
    values = all_channels(carrier())
    fake = FakeDevice(
        state="ready",
        batches=split_batches(values),
        read_error=o.DeviceTimeoutError("read timeout"),
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.DeviceTimeoutError, match="timed out"):
        board.measure_impedance(timeout=1.0)
    assert control_calls(fake).count("stop") == 2
    assert configure_calls(fake)[-1]["loff_mask"] == 0
    assert board.get_config().loff_mask == 0
    assert fake.state == "ready"


def test_measurement_stop_failure_reports_unconfirmed_loff():
    values = all_channels(carrier())
    fake = FakeDevice(
        state="ready",
        batches=split_batches(values),
        stop_failures=[None, o.TransportError("stop failed")],
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.TransportError, match="stop failed") as error:
        board.measure_impedance(timeout=1.0)
    note = " ".join(error.value.__notes__)
    assert "LOFF" in note
    assert "may remain enabled" in note
    assert fake.state == "faulted"
    assert board._closed


def test_restore_failure_reports_unconfirmed_loff_and_primary_error():
    values = all_channels(carrier())
    fake = FakeDevice(
        state="ready",
        batches=split_batches(values),
        configure_failures=[
            None,
            o.ConfigurationError("restore rejected"),
            o.ConfigurationError("restore rejected again"),
        ],
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(o.ConfigurationError, match="restore rejected") as error:
        board.measure_impedance(timeout=1.0)
    note = " ".join(error.value.__notes__)
    assert "LOFF" in note
    assert "may remain enabled" in note
    assert fake.state == "ready"
    assert fake.config["loff_mask"] == 0xFFFFFFFF
    last_configure = max(
        index for index, (name, _) in enumerate(fake.calls) if name == "configure"
    )
    assert "start" not in [name for name, _ in fake.calls[last_configure + 1 :]]
    assert board._closed


def test_keyboard_interrupt_still_stops_and_restores():
    fake = FakeDevice(
        state="ready",
        read_error=KeyboardInterrupt("interrupted"),
    )
    board = o.Board(fake, "stm32")
    with pytest.raises(KeyboardInterrupt, match="interrupted"):
        board.measure_impedance(timeout=1.0)
    assert control_calls(fake).count("stop") == 2
    assert configure_calls(fake)[-1]["loff_mask"] == 0
    assert board.get_config().loff_mask == 0
    assert fake.state == "ready"


def test_stop_not_confirmed_after_read_is_reported():
    values = all_channels(carrier())
    fake = FakeDevice(state="ready", batches=split_batches(values))
    stops = 0

    def stop_without_ready(timeout):
        nonlocal stops
        stops += 1
        fake.calls.append(("stop", None))
        fake.state = "streaming" if stops >= 2 else "ready"

    fake.stop = stop_without_ready
    board = o.Board(fake, "stm32")
    with pytest.raises(o.InvalidStateError, match="not confirmed") as error:
        board.measure_impedance(timeout=1.0)
    note = " ".join(error.value.__notes__)
    assert "LOFF" in note
    assert "may remain enabled" in note
    assert board._closed
