"""python examples/discover.py --transport ble"""

import argparse

from omnibci import Board

parser = argparse.ArgumentParser()
parser.add_argument("--transport", choices=("serial", "ble"), default="serial")
args = parser.parse_args()
for device in Board.discover(transport=args.transport):
    print(device.endpoint, device.name, device.rssi_dbm)
