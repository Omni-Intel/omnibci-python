"""python examples/impedance.py serial://COM5 --mask 0xFF"""

import argparse

from omnibci import Board

parser = argparse.ArgumentParser()
parser.add_argument("endpoint")
parser.add_argument("--mask", type=lambda value: int(value, 0), default=None)
parser.add_argument("--series-kohm", type=float, default=None)
parser.add_argument("--timeout", type=float, default=15.0)
args = parser.parse_args()

with Board.connect(args.endpoint) as board:
    result = board.measure_impedance(
        mask=args.mask,
        series_kohm=args.series_kohm,
        timeout=args.timeout,
    )
    print(f"mask: 0x{result.mask:02X}")
    print(f"series: {result.series_kohm:.2f} kOhm")
    for channel, value in enumerate(result.values_kohm, start=1):
        print(channel, "invalid" if value is None else f"{value:.2f} kOhm")
    if result.clipped_mask:
        print(f"clipped: 0x{result.clipped_mask:02X}")
    # 测量后板子保持停止；需要继续采集时显式调用 board.start()。
