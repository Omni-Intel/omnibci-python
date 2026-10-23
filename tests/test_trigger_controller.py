import pytest
from omnibci import TriggerController, _native


class Port:
    def __init__(self):
        self.open = True
        self.sent = []

    def send(self, code):
        if not self.open:
            raise RuntimeError("closed")
        self.sent.append(code)

    def close(self):
        self.open = False

    def is_open(self):
        return self.open


def test_trigger_context_and_codes(monkeypatch):
    port = Port()

    class Factory:
        @staticmethod
        def connect(name, timeout):
            assert (name, timeout) == ("COM_TEST", 1.0)
            return port

    monkeypatch.setattr(_native, "NativeTriggerController", Factory)
    with TriggerController.connect("COM_TEST") as controller:
        assert controller.is_open
        for code in (0, 7, 7, 255):
            controller.send(code)
        for bad in (-1, 256, True, 1.5, "7", None):
            with pytest.raises(ValueError):
                controller.send(bad)
    assert port.sent == [0, 7, 7, 255]
    assert not controller.is_open
    controller.close()
    with pytest.raises(RuntimeError):
        controller.send(1)


def test_trigger_exception_releases_port(monkeypatch):
    port = Port()

    class Factory:
        @staticmethod
        def connect(name, timeout):
            return port

    monkeypatch.setattr(_native, "NativeTriggerController", Factory)
    with pytest.raises(RuntimeError):
        with TriggerController("COM_TEST"):
            raise RuntimeError("experiment failed")
    assert not port.open


@pytest.mark.parametrize(
    "port,timeout",
    [("", 1), (" ", 1), (None, 1), ("x", 0), ("x", float("nan")), ("x", 61)],
)
def test_trigger_invalid_arguments_do_not_open(port, timeout):
    with pytest.raises(ValueError):
        TriggerController(port, timeout=timeout)
