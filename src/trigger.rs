//! Python ownership wrapper for the shared SDK serial trigger controller.
use omnibci_sdk::trigger::TriggerController;
use pyo3::prelude::*;
use std::sync::Mutex;

#[pyclass(frozen, module = "omnibci._native")]
pub(crate) struct NativeTriggerController {
    controller: Mutex<TriggerController>,
}

#[pymethods]
impl NativeTriggerController {
    #[staticmethod]
    fn connect(py: Python<'_>, port: String, timeout: f64) -> PyResult<Self> {
        let timeout = super::duration(timeout)?;
        let controller = py
            .detach(|| TriggerController::connect(&port, timeout))
            .map_err(|e| super::TransportError::new_err(e.to_string()))?;
        Ok(Self {
            controller: Mutex::new(controller),
        })
    }
    #[staticmethod]
    fn available_ports(py: Python<'_>) -> PyResult<Vec<String>> {
        py.detach(TriggerController::available_ports)
            .map_err(|e| super::TransportError::new_err(e.to_string()))
    }
    fn send(&self, py: Python<'_>, code: u16) -> PyResult<()> {
        if code > 255 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "code must be in [0, 255]",
            ));
        }
        py.detach(|| {
            self.controller
                .lock()
                .map_err(|_| super::TransportError::new_err("trigger lock poisoned"))?
                .send_code(code)
                .map_err(|e| super::TransportError::new_err(e.to_string()))
        })
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.controller
                .lock()
                .map_err(|_| super::TransportError::new_err("trigger lock poisoned"))?
                .close();
            Ok(())
        })
    }
    fn is_open(&self) -> PyResult<bool> {
        Ok(self
            .controller
            .lock()
            .map_err(|_| super::TransportError::new_err("trigger lock poisoned"))?
            .is_open())
    }
}
