"""Lead-off impedance estimation; all device control stays in Board.

The GUI estimates the 31.25 Hz lead-off carrier with a sine/cosine/DC least
squares fit. The ADS1299 alternates +/-6 nA, so the fundamental peak is
4*6/pi nA. A raw estimate is divided by that current floor and then reduced
by the series resistance. Channels without a full second of finite valid
samples, or with any full-scale ADC code in the window, stay invalid.
"""

from __future__ import annotations

import math

import numpy as np
from numpy.typing import NDArray

LEAD_OFF_FREQUENCY_HZ = 31.25
# ADS1299 AC lead-off alternates +/-6 nA; the fundamental peak is 4I/pi.
# TI: https://www.ti.com/lit/ds/symlink/ads1299.pdf (lead-off detection)
LEAD_OFF_FUNDAMENTAL_NA = 4.0 * 6.0 / math.pi
DEFAULT_SERIES_KOHM = 9.98
WINDOW_SECONDS = 4
MIN_VALID_SAMPLES_PER_SECOND = 1
CHANNELS = 32
FULL_SCALE_MIN = -0x800000
FULL_SCALE_MAX = 0x7FFFFF
_MAX_SERIES_KOHM = 1000.0


def _compensate(raw_kohm: float, series_kohm: float) -> float | None:
    if not math.isfinite(raw_kohm) or raw_kohm < 0.0:
        return None
    value = raw_kohm - series_kohm
    return value if math.isfinite(value) else None


def _solve_3x3(matrix: list[list[float]], rhs: list[float]) -> list[float] | None:
    """Solve the sine/cosine/DC normal equations like the GUI solver."""
    for pivot in range(3):
        best = max(range(pivot, 3), key=lambda row: abs(matrix[row][pivot]))
        if abs(matrix[best][pivot]) < 1.0e-12:
            return None
        matrix[pivot], matrix[best] = matrix[best], matrix[pivot]
        rhs[pivot], rhs[best] = rhs[best], rhs[pivot]
        pivot_row = matrix[pivot]
        for row in range(pivot + 1, 3):
            factor = matrix[row][pivot] / pivot_row[pivot]
            for column in range(pivot, 3):
                matrix[row][column] -= factor * pivot_row[column]
            rhs[row] -= factor * rhs[pivot]
    solution = [0.0, 0.0, 0.0]
    for row in (2, 1, 0):
        remainder = sum(
            matrix[row][column] * solution[column] for column in range(row + 1, 3)
        )
        solution[row] = (rhs[row] - remainder) / matrix[row][row]
    return solution


def estimate_impedance_kohm(
    eeg_uv: NDArray[np.float32],
    raw_counts: NDArray[np.int32],
    sequence: NDArray[np.uint32],
    valid: NDArray[np.bool_],
    *,
    mask: int,
    series_kohm: float = DEFAULT_SERIES_KOHM,
    sample_rate_hz: int,
) -> tuple[tuple[float | None, ...], int]:
    """Fit one window and return per-channel kOhm plus the clipped mask.

    ``sequence`` supplies the phase reference, so dropped frames leave a gap
    instead of compressing the carrier. Only selected channels are fitted;
    the clipped mask is likewise restricted to ``mask``.
    """
    values = np.asarray(eeg_uv)
    counts = np.asarray(raw_counts)
    indices = np.asarray(sequence)
    usable = np.asarray(valid, dtype=bool)
    if (
        values.ndim != 2
        or values.shape[1] != CHANNELS
        or counts.shape != values.shape
        or indices.shape != (values.shape[0],)
        or usable.shape != (values.shape[0],)
    ):
        raise ValueError("impedance window arrays do not share a 32-channel shape")
    if type(mask) is not int or not 0 <= mask <= 0xFFFFFFFF:
        raise ValueError("mask must be an integer in [0, 4294967295]")
    if (
        isinstance(series_kohm, bool)
        or not isinstance(series_kohm, (int, float))
        or not 0.0 <= series_kohm <= _MAX_SERIES_KOHM
    ):
        raise ValueError("series_kohm must be finite and within [0, 1000]")
    if type(sample_rate_hz) is not int or sample_rate_hz <= 0:
        raise ValueError("sample_rate_hz must be a positive integer")
    if values.shape[0] == 0:
        return (None,) * CHANNELS, 0

    # Full-scale ADS1299 codes reject the channel. Only valid rows count,
    # matching the GUI's window scan.
    clipped = np.zeros(CHANNELS, dtype=bool)
    if usable.any():
        valid_counts = counts[usable]
        clipped = np.any(
            (valid_counts <= FULL_SCALE_MIN) | (valid_counts >= FULL_SCALE_MAX),
            axis=0,
        )
    clipped_mask = sum(1 << ch for ch in range(CHANNELS) if clipped[ch]) & mask

    first = int(indices[0])
    # Differences stay small inside one window; the mask keeps u32 wrap.
    offsets = ((indices.astype(np.int64) - first) & 0xFFFFFFFF).astype(np.float64)
    phase = 2.0 * math.pi * LEAD_OFF_FREQUENCY_HZ * offsets / sample_rate_hz
    basis = np.column_stack((np.sin(phase), np.cos(phase), np.ones_like(phase)))

    minimum_samples = sample_rate_hz * MIN_VALID_SAMPLES_PER_SECOND
    output: list[float | None] = [None] * CHANNELS
    for channel in range(CHANNELS):
        if mask & (1 << channel) == 0 or clipped_mask & (1 << channel):
            continue
        keep = usable & np.isfinite(values[:, channel])
        if int(keep.sum()) < minimum_samples:
            continue
        column = values[keep, channel].astype(np.float64)
        if column.min() == column.max():
            continue
        selected = basis[keep]
        normal = selected.T @ selected
        rhs = selected.T @ column
        coefficient = _solve_3x3(normal.tolist(), rhs.tolist())
        if coefficient is None:
            continue
        carrier_peak_uv = math.hypot(coefficient[0], coefficient[1])
        raw_kohm = carrier_peak_uv / LEAD_OFF_FUNDAMENTAL_NA
        compensated = _compensate(raw_kohm, float(series_kohm))
        if compensated is not None:
            output[channel] = max(compensated, 0.0)
    return tuple(output), clipped_mask
