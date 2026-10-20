"""Direct OmniBCI acquisition backed by the Rust SDK."""

from . import _native
from .api import (
    Board,
    ComponentFirmware,
    DeviceInfo,
    DeviceMetadata,
    FrontendConfig,
    PartialReadError,
    SampleBatch,
    Snapshot,
    Stm32Config,
    decode_frames,
)
from .trigger import TriggerController

OmniBCIError = _native.OmniBCIError
DeviceTimeoutError = _native.DeviceTimeoutError
InvalidStateError = _native.InvalidStateError
ConfigurationError = _native.ConfigurationError
DisconnectedError = _native.DisconnectedError
BufferOverflowError = _native.BufferOverflowError
TransportError = _native.TransportError
__version__ = "0.2.0"
SDK_VERSION = _native.SDK_VERSION
SDK_REVISION = _native.SDK_REVISION
__all__ = [
    "Board",
    "TriggerController",
    "ComponentFirmware",
    "DeviceInfo",
    "DeviceMetadata",
    "FrontendConfig",
    "Stm32Config",
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
