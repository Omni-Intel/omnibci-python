"""Measure STM32 32-channel impedance from an endpoint selected by the user."""

import argparse

from omnibci import Board


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", help="ble://KEY or serial://PORT to measure")
    parser.add_argument("--transport", choices=("ble", "serial"), default="serial")
    parser.add_argument("--mask", type=lambda value: int(value, 0), default=None)
    parser.add_argument("--series-kohm", type=float, default=9.98)
    parser.add_argument("--window-seconds", type=float, default=4.0)
    parser.add_argument("--timeout", type=float, default=15.0)
    args = parser.parse_args()
    if args.endpoint is None:
        for device in Board.discover(transport=args.transport, device_type="stm32"):
            print(device.name, device.endpoint, device.rssi_dbm)
        return
    with Board.connect(args.endpoint, device_type="stm32", timeout=20) as board:
        result = board.measure_impedance(
            mask=args.mask,
            series_kohm=args.series_kohm,
            window_seconds=args.window_seconds,
            timeout=args.timeout,
        )
        for channel, value in enumerate(result.values_kohm, start=1):
            print(channel, "None" if value is None else f"{value:.2f} kOhm")
        print("clipped mask:", hex(result.clipped_mask))


if __name__ == "__main__":
    main()
