import math
from dataclasses import replace

import numpy as np
import omnibci as o
import pytest
from omnibci import impedance
from test_impedance import (
    DEFAULT_CONFIG,
    FakeDevice,
    all_channels,
    carrier,
    split_batches,
    wire,
)


def board_for(fake):
    return o.Board(fake, device_type="stm32")


@pytest.mark.parametrize("seconds", [1, 1.002, 2.5, 4, 8, 60])
def test_window_duration_rounds_up_and_restores(seconds, monkeypatch):
    slots = math.ceil(250 * seconds)
    positions = np.arange(slots + 17)
    fake = FakeDevice(
        state="ready", batches=[wire(all_channels(carrier(positions=positions)))]
    )
    board = board_for(fake)
    observed = {}
    estimate = impedance.estimate_impedance_kohm

    def inspect(uv, counts, sequence, valid, **kwargs):
        observed["sequence"] = sequence.copy()
        return estimate(uv, counts, sequence, valid, **kwargs)

    monkeypatch.setattr(impedance, "estimate_impedance_kohm", inspect)
    result = board.measure_impedance(window_seconds=seconds, mask=1)
    assert len(observed["sequence"]) == slots
    assert observed["sequence"][0] == 17
    assert observed["sequence"][-1] == slots + 16
    assert result.values_kohm[0] == pytest.approx(22.5, abs=1e-3)
    assert board.get_config() == replace(DEFAULT_CONFIG, verified=True)
    assert fake.state == "ready"


@pytest.mark.parametrize(
    "kwargs,expected_batches",
    [({}, 4), ({"window_seconds": 2}, 2), ({"window_seconds": 8}, 8)],
)
def test_window_controls_collection_not_timeout(kwargs, expected_batches):
    fake = FakeDevice(
        state="ready",
        batches=split_batches(all_channels(carrier(positions=np.arange(2500)))),
    )
    board_for(fake).measure_impedance(timeout=15, **kwargs)
    stops = [i for i, call in enumerate(fake.calls) if call[0] == "stop"]
    assert (
        sum(call[0] == "read" for call in fake.calls[stops[0] : stops[1]])
        == expected_batches
    )


@pytest.mark.parametrize(
    "value",
    [
        True,
        False,
        None,
        "4",
        0,
        -1,
        0.99,
        60.01,
        float("nan"),
        float("inf"),
        -float("inf"),
    ],
)
def test_invalid_window_does_not_touch_hardware(value):
    fake = FakeDevice(state="ready")
    with pytest.raises(ValueError, match="window_seconds"):
        board_for(fake).measure_impedance(window_seconds=value)
    assert fake.calls == []


def test_custom_window_failure_restores_configuration():
    fake = FakeDevice(state="ready", batches=split_batches(all_channels(carrier())))
    board = board_for(fake)
    with pytest.raises(o.DeviceTimeoutError):
        board.measure_impedance(window_seconds=8, timeout=10)
    assert board.get_config() == replace(DEFAULT_CONFIG, verified=True)
    assert fake.state == "ready"
