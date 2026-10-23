import json
from dataclasses import replace

import numpy as np
import omnibci as o
import pytest
from test_impedance import (
    FakeDevice,
    all_channels,
    carrier,
    configure_calls,
    fit_window,
    lead_off_calls,
    split_batches,
    wire,
)


def test_rank_deficient_carrier_window_is_not_an_impedance():
    # Every retained sample has the same carrier phase: amplitude cannot be identified.
    positions = np.arange(1000, dtype=np.uint32) * 8
    values = np.arange(1000, dtype=np.float32)
    result, _ = fit_window(values, sequence=positions)
    assert result[0] is None


def test_real_u32_wrap_keeps_carrier_amplitude():
    positions = np.arange(1000, dtype=np.uint64)
    sequence = ((positions + 2**32 - 300) % 2**32).astype(np.uint32)
    result, _ = fit_window(carrier(positions=positions), sequence=sequence)
    assert result[0] == pytest.approx(22.5, abs=1e-3)


def test_restore_retains_custom_configuration_but_disables_loff():
    config = o.FrontendConfig(
        reference="srb2",
        mode="no_bias",
        enabled_mask=0x35,
        bias_mask=0x11,
        srb2_mask=0x04,
        gains=tuple(([1, 2, 4, 6, 8, 12, 24] * 2)[:8]),
    )
    fake = FakeDevice(
        state="ready", config=config, batches=split_batches(all_channels(carrier()))
    )
    board = o.Board(fake)
    result = board.measure_impedance(mask=0x25)

    assert lead_off_calls(fake) == [0x25, 0]
    assert configure_calls(fake)[0] == dict(
        config._wire(), bias_mask=0, gains=list(config.gains)
    )
    assert configure_calls(fake)[-1] == dict(config._wire(), gains=list(config.gains))
    assert result.mask == 0x25
    assert result.series_kohm == pytest.approx(4.40)
    assert board.get_config() == replace(config, verified=True)


def test_gap_outside_last_four_seconds_is_not_fitted():
    early = wire(
        all_channels(carrier(total_kohm=1000, positions=np.arange(250))),
        indices=np.arange(250),
        sequence=np.arange(250),
    )
    positions = np.arange(1100, 2100)
    latest = wire(
        all_channels(carrier(positions=positions)),
        indices=positions,
        sequence=positions,
    )
    fake = FakeDevice(state="ready", batches=[early, latest])
    result = o.Board(fake).measure_impedance(mask=1)
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)


def test_measurement_owns_board_until_restoration_finishes():
    import threading

    entered = threading.Event()
    release = threading.Event()
    observed = threading.Event()
    fake = FakeDevice(state="ready", batches=split_batches(all_channels(carrier())))
    original_read = fake.read

    def blocking_read(timeout):
        entered.set()
        assert release.wait(2)
        return original_read(timeout)

    fake.read = blocking_read
    board = o.Board(fake)
    errors = []

    def measure():
        try:
            board.measure_impedance()
        except BaseException as error:
            errors.append(error)

    def inspect():
        assert board.get_config().bias_mask == 0xFF
        observed.set()

    measurement = threading.Thread(target=measure)
    observation = threading.Thread(target=inspect)
    measurement.start()
    try:
        assert entered.wait(2)
        observation.start()
        assert not observed.wait(0.05)
    finally:
        release.set()
        measurement.join(2)
        observation.join(2)
    assert not measurement.is_alive()
    assert not observation.is_alive()
    assert not errors
    assert observed.is_set()


def test_restore_readback_mismatch_is_reported():
    fake = FakeDevice(state="ready", batches=split_batches(all_channels(carrier())))

    def mismatching_restore(config, timeout):
        parsed = json.loads(config)
        fake.calls.append(("configure", parsed))
        if parsed["bias_mask"] == 0:
            fake.config = dict(parsed, verified=True)

    fake.configure = mismatching_restore
    board = o.Board(fake)
    with pytest.raises(o.TransportError, match="readback") as error:
        board.measure_impedance()
    note = " ".join(error.value.__notes__)
    assert "LOFF" in note
    assert "original configuration" in note
    assert board._closed


def test_unconfirmed_restoration_isolates_failed_session():
    fake = FakeDevice(
        state="ready",
        batches=split_batches(all_channels(carrier())),
        configure_failures=[
            None,
            o.ConfigurationError("restore failed"),
            o.ConfigurationError("restore failed"),
        ],
    )
    board = o.Board(fake)
    with pytest.raises(o.ConfigurationError):
        board.measure_impedance()
    # GUI closes a failed transaction rather than allowing EEG with uncertain LOFF.
    with pytest.raises(o.DisconnectedError):
        board.start()
    assert any(name == "close" for name, _ in fake.calls)


def test_two_second_delivery_gap_resets_measurement_window():
    batches = split_batches(all_channels(carrier(positions=np.arange(1250))))
    batches[0] = wire(all_channels(carrier(total_kohm=500, positions=np.arange(250))))
    batches[0]["received_age_s"] = 3.0
    result = o.Board(FakeDevice(state="ready", batches=batches)).measure_impedance(
        mask=1
    )
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)


def test_stale_complete_batch_is_not_returned_as_current_impedance():
    stale = wire(all_channels(carrier(total_kohm=500)))
    stale["received_age_s"] = 3.0
    fresh = wire(
        all_channels(carrier()),
        sequence=np.arange(1000, 2000),
        indices=np.arange(1000, 2000),
    )
    result = o.Board(
        FakeDevice(state="ready", batches=[stale, fresh])
    ).measure_impedance(mask=1)
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)


def test_delivery_gap_resets_accumulated_window():
    early = wire(
        all_channels(carrier(total_kohm=500, positions=np.arange(500))),
        indices=np.arange(500),
        sequence=np.arange(500),
    )
    latest_positions = np.arange(500, 1500)
    latest = wire(
        all_channels(carrier(positions=latest_positions)),
        indices=latest_positions,
        sequence=latest_positions,
    )
    # A host timestamp jump makes this delivery more than two seconds later.
    latest["received_age_s"] = -2.5
    result = o.Board(
        FakeDevice(state="ready", batches=[early, latest])
    ).measure_impedance(mask=1)
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)


def test_mid_window_stale_batch_discards_partial_window():
    first = wire(
        all_channels(carrier(total_kohm=500, positions=np.arange(500))),
        indices=np.arange(500),
        sequence=np.arange(500),
    )
    stale = wire(
        all_channels(carrier(total_kohm=500, positions=np.arange(500, 1000))),
        indices=np.arange(500, 1000),
        sequence=np.arange(500, 1000),
    )
    stale["received_age_s"] = 3.0
    fresh_positions = np.arange(1000, 2000)
    fresh = wire(
        all_channels(carrier(positions=fresh_positions)),
        indices=fresh_positions,
        sequence=fresh_positions,
    )
    result = o.Board(
        FakeDevice(state="ready", batches=[first, stale, fresh])
    ).measure_impedance(mask=1)
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)


def test_fresh_batches_separated_by_delivery_outage_reset_window(monkeypatch):
    import omnibci.api as api

    clock = [100.0]
    batches = split_batches(all_channels(carrier(positions=np.arange(1250))))
    batches[0] = wire(all_channels(carrier(total_kohm=500, positions=np.arange(250))))
    fake = FakeDevice(state="ready", batches=batches)
    read = fake.read
    reads = [0]

    def delayed_read(timeout):
        reads[0] += 1
        if reads[0] == 2:
            clock[0] += 2.1
        return read(timeout)

    fake.read = delayed_read
    monkeypatch.setattr(api.time, "monotonic", lambda: clock[0])
    result = o.Board(fake).measure_impedance(mask=1)
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)
