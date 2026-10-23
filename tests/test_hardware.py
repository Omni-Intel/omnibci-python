"""Opt in with OMNIBCI_TEST_ENDPOINT; this test starts real acquisition."""

import os

import pytest
from omnibci import Board


@pytest.mark.hardware
@pytest.mark.skipif(
    not os.environ.get("OMNIBCI_TEST_ENDPOINT"), reason="real board not requested"
)
def test_real_board_acquisition():
    device_type = os.environ.get("OMNIBCI_TEST_DEVICE_TYPE", "esp32")
    with Board.connect(
        os.environ["OMNIBCI_TEST_ENDPOINT"], device_type=device_type, timeout=20
    ) as board:
        assert board.get_config().verified
        board.start()
        batch = board.read(250, timeout=10)
        assert batch.eeg_uv.shape == (250, 32 if device_type == "stm32" else 8)
        board.stop()
