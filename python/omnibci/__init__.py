"""Direct OmniBCI acquisition backed by the Rust SDK."""

from . import _native
from .api import (
    Board,
    DeviceInfo,
    DeviceMetadata,
    FrontendConfig,
    PartialReadError,
    SampleBatch,
    Snapshot,
    decode_frames,
)

OmniBCIError = _native.OmniBCIError
DeviceTimeoutError = _native.DeviceTimeoutError
InvalidStateError = _native.InvalidStateError
ConfigurationError = _native.ConfigurationError
DisconnectedError = _native.DisconnectedError
BufferOverflowError = _native.BufferOverflowError
TransportError = _native.TransportError
__version__ = "0.1.0"
SDK_VERSION = _native.SDK_VERSION
SDK_REVISION = _native.SDK_REVISION
__all__ = [
    "Board",
    "DeviceInfo",
    "DeviceMetadata",
    "FrontendConfig",
    "SampleBatch",
    "Snapshot",
    "PartialReadError",
    "decode_frames",
    "OmniBCIError",
    "DeviceTimeoutError",
    "InvalidStateError",
    "ConfigurationError",
    "DisconnectedError",
    "BufferOverflowError",
    "TransportError",
    "SDK_VERSION",
    "SDK_REVISION",
]
