# Hardware trigger reception and dispatch

The SDK pin is `e1f25b3a6d272e5040929c53b331d45738ff6d73`. Current STM32
acquisition uses the board USB CDC port or E73 BLE only; the ESB Dongle and
dual-mode switching were removed from the firmware. Both transports use the same
SDK protocol driver. External serial trigger control is separate from acquisition
(`TriggerController`).

`Board.read_triggers()` drains currently available STM32 events without waiting.
Each immutable `TriggerEvent` has `generation`, `code`, anchor `sequence`, legacy
`source_timestamp_ms` and optional `timing` (`trigger_timestamp_us` and
`anchor_timestamp_us`). Python integers retain all 64 bits. The source timestamp
is the trigger-start device time, not host arrival time. No rounding to an EEG
sample grid is performed.

`SampleBatch.source_timestamp_us` contains uint32 DRDY device times, with
`source_timestamp_valid` marking availability. Unavailable times (e.g. ESP32) are
zero with a false validity mask; zero itself is a valid wrapped timestamp when the
mask is true. Arrays retain read-only ownership through fixed reads and slicing.
Compute adjacent timestamp differences modulo 2^32; associate events with their
anchor sequence, even when the event arrives in a later read batch.

STM32 32.2.4 aligns exported EEG and trigger timestamps to the nominal ADS clock
after a startup settling interval. Wire fields and units are unchanged. Before a
timing experiment, acquire uninterrupted for at least one second and verify the
firmware clock diagnostic `ready=1`. This is a monotonic, ADC-referenced timebase;
it makes no UTC or host-clock accuracy claim. See the firmware
[timing reference](https://github.com/Omni-Intel/omnibci-firmware-stm32/blob/main/stm32/TIMING.md).

The event queue is independent of EEG reads and holds 16,384 events. Poll both
queues regularly. Overflow faults the acquisition rather than silently discarding
continuation: retained events can be drained, then the next empty read reports the
fault. STOP preserves events, and a successfully acknowledged START clears the old
generation. Drain tail EEG and triggers before close. ESP32 hardware event polling
is currently unsupported and returns an explicit configuration error; normal ESP32
sample acquisition remains supported.

`decode_triggers(bytes)` decodes complete STM32 event packets offline. Each call
creates a fresh parser; incomplete trailing packets and CRC-invalid packets are
not delivered. Use `Board` for live streaming.

`TriggerController.send(code)` accepts 0..255, including zero and repeated codes,
and confirms host write/flush only. Actual device reception is verified through
`Board.read_triggers()`. Do not replace source timing with send or receive time.

Historical hardware acceptance (2026-10-21): with STM32 32.2.3 and E73 0.5.1, USB
and BLE each delivered 1,000/1,000 events, with no EEG gaps, CRC errors or
timestamp-anchor mismatches. That run used SDK
`aa9ab512ecf6243646fbbb23e9432e3c79eac1ec`, not the current pin, and was short-run
Windows acceptance; Linux/macOS hardware was not tested. ESB Dongle hardware was
not available then and is retired from current firmware.
Raw local evidence: `D:/workspace/trigger-validation/cc-bindings`.

Current upgrade validation: the bindings were upgraded to `e1f25b3...` and
rebuilt as a release abi3 extension; 22 native Rust tests and 66 Python tests
passed (hardware excluded).
`cargo fmt --check` and `cargo clippy --locked -- -D warnings` passed. New SDK
hardware tests were not rerun, so the historical acceptance above does not validate
the current pin. Software tests still cover exact source timestamps above 2^53,
uint32 wrap, repeated/zero codes, multiple events per anchor, fragmented event-only
input, bounded queue overflow and stop/start.
