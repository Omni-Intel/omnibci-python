"""Send one hardware trigger and read its source timing (STM32 USB or BLE).

Example: python examples/trigger_stm32.py serial://COM18 COM23
For BLE use the endpoint from Board.discover(device_type="stm32", transport="ble").
"""

import argparse
import time

from omnibci import Board, TriggerController

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("endpoint")
parser.add_argument("trigger_port")
args = parser.parse_args()
with Board.connect(args.endpoint, device_type="stm32", timeout=25) as board:
    with TriggerController(args.trigger_port) as trigger:
        board.start()
        try:
            board.read(125, timeout=3)  # establish actual EEG history
            trigger.send(7)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                batch = board.read(timeout=0.5)
                # Retain batch.sequence/source_timestamp_us for later event alignment.
                for event in board.read_triggers():
                    print(event)
        finally:
            board.stop()
            for event in board.read_triggers():
                print("tail", event)
