"""Serial trigger dispatch using the Rust SDK; no Python serial implementation."""

from __future__ import annotations

from . import _native
from .api import _timeout


class TriggerController:
    """Own a configurable 10000 baud 8N1 trigger port.

    send() confirms host dispatch only. Read the acquisition device's hardware
    trigger events for device timestamps and confirmation of reception.
    """

    def __init__(self, port: str, *, timeout: float = 1.0):
        if not isinstance(port, str) or not port.strip():
            raise ValueError("port must be a nonempty string")
        self._controller = _native.NativeTriggerController.connect(
            port, _timeout(timeout)
        )

    @classmethod
    def connect(cls, port: str, *, timeout: float = 1.0) -> TriggerController:
        return cls(port, timeout=timeout)

    @staticmethod
    def available_ports() -> list[str]:
        """Return serial candidates; these are not verified trigger generators."""
        return _native.NativeTriggerController.available_ports()

    @property
    def is_open(self) -> bool:
        return self._controller.is_open()

    def send(self, code: int) -> None:
        if type(code) is not int or not 0 <= code <= 255:
            raise ValueError("code must be an integer in [0, 255]")
        self._controller.send(code)

    def close(self) -> None:
        self._controller.close()

    def __enter__(self) -> TriggerController:
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        self.close()
