"""List STM32 candidates, or explicitly acquire from an endpoint selected by the user."""

import argparse

from omnibci import Board


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", help="ble://KEY or serial://PORT to acquire from")
    parser.add_argument("--transport", choices=("ble", "serial"), default="ble")
    args = parser.parse_args()
    if args.endpoint is None:
        for device in Board.discover(transport=args.transport, device_type="stm32"):
            print(device.name, device.endpoint, device.rssi_dbm)
        return
    with Board.connect(args.endpoint, device_type="stm32", timeout=20) as board:
        print(board.info)
        print(board.get_config())
        board.start()
        batch = board.read(250, timeout=5)
        print("shape:", batch.eeg_uv.shape)
        print("received sample indices:", batch.sample_indices)
        board.stop()


if __name__ == "__main__":
    main()
