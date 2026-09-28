"""python examples/acquire.py serial://COM5 --samples 250"""

import argparse

from omnibci import Board

parser = argparse.ArgumentParser()
parser.add_argument("endpoint")
parser.add_argument("--samples", type=int, default=250)
args = parser.parse_args()
with Board.connect(args.endpoint) as board:
    print(board.info)
    board.start()
    batch = board.read(args.samples, timeout=10)
    board.stop()
    print("microvolts:", batch.eeg_uv)
    print("sample indices:", batch.sample_indices)
    print("quality:", board.snapshot)
