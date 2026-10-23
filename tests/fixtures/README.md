# Synthetic STM32 timing fixture

`stm32-trigger-timing.bin` is synthetic protocol data, not a human EEG recording.
It contains two valid 128-byte EEG frames (sequence `0xffffffff`, `0`; DRDY low
32-bit time `0xfffff830`, `2000`) and four valid 48-byte hardware trigger packets.
All triggers anchor to sequence 0, with codes 0, 7, 7, 255 and trigger timestamps
`2^53 + 2^32 + 2000 - offset`, where offsets are 1234, 1000, 500, 1 microseconds.
All CRCs use CCITT with initial value 0xffff. EEG mode is zero, packet byte 24 is
3, byte 125 is 4, and each ADS status word starts with 0xc0. Counts are zero.

The fixture tests uint32 wrap, exact uint64 decoding beyond double precision,
multiple events sharing an anchor, repeated/zero codes, truncation and CRC rejection.
