//! Thin CPython adapter; all protocol and device operations remain in omnibci-sdk.
use omnibci_sdk::{
    protocol::FrontendConfig,
    session::{Device, Error, Samples},
};
use pyo3::{
    create_exception,
    exceptions::PyValueError,
    prelude::*,
    types::{PyBytes, PyDict},
};
use serde::Deserialize;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

create_exception!(_native, OmniBCIError, pyo3::exceptions::PyException);
create_exception!(_native, DeviceTimeoutError, OmniBCIError);
create_exception!(_native, InvalidStateError, OmniBCIError);
create_exception!(_native, ConfigurationError, OmniBCIError);
create_exception!(_native, DisconnectedError, OmniBCIError);
create_exception!(_native, BufferOverflowError, OmniBCIError);
create_exception!(_native, TransportError, OmniBCIError);
fn map_error(error: Error) -> PyErr {
    let message = error.to_string();
    match error {
        Error::Timeout => DeviceTimeoutError::new_err(message),
        Error::InvalidState => InvalidStateError::new_err(message),
        Error::InvalidConfig(_) => ConfigurationError::new_err(message),
        Error::Disconnected => DisconnectedError::new_err(message),
        Error::Overflow => BufferOverflowError::new_err(message),
        Error::Transport(_) | Error::WorkerPanicked => TransportError::new_err(message),
    }
}
fn duration(seconds: f64) -> PyResult<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > 60.0 {
        return Err(PyValueError::new_err(
            "timeout must be finite and within (0, 60] seconds",
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    reference: u8,
    mode: u8,
    enabled_mask: u8,
    bias_mask: u8,
    srb2_mask: u8,
    gains: [u8; 8],
}
impl From<Config> for FrontendConfig {
    fn from(c: Config) -> Self {
        Self {
            reference: c.reference,
            mode: c.mode,
            enabled_mask: c.enabled_mask,
            bias_mask: c.bias_mask,
            srb2_mask: c.srb2_mask,
            gains: c.gains,
            verified: false,
        }
    }
}
#[pyclass(frozen, module = "omnibci._native")]
struct NativeDevice {
    device: Mutex<Option<Device>>,
}
impl NativeDevice {
    fn with<T: Send>(
        &self,
        py: Python<'_>,
        operation: impl FnOnce(&mut Device) -> Result<T, Error> + Send,
    ) -> PyResult<T> {
        py.detach(|| {
            let mut guard = self.device.lock().map_err(|_| Error::WorkerPanicked)?;
            let device = guard.as_mut().ok_or(Error::Disconnected)?;
            operation(device)
        })
        .map_err(map_error)
    }
}
#[pymethods]
impl NativeDevice {
    #[staticmethod]
    fn connect(py: Python<'_>, endpoint: &str, timeout: f64) -> PyResult<Self> {
        let timeout = duration(timeout)?;
        let (kind, address) = endpoint
            .split_once("://")
            .ok_or_else(|| PyValueError::new_err("endpoint must be serial://PORT or ble://KEY"))?;
        if address.is_empty() || !matches!(kind, "serial" | "ble") {
            return Err(PyValueError::new_err(
                "endpoint must be serial://PORT or ble://KEY",
            ));
        }
        let device = py
            .detach(|| match kind {
                "serial" => Device::connect_serial(address, timeout),
                _ => Device::connect_ble(address, timeout),
            })
            .map_err(map_error)?;
        Ok(Self {
            device: Mutex::new(Some(device)),
        })
    }
    fn start(&self, py: Python<'_>, timeout: f64) -> PyResult<()> {
        let timeout = duration(timeout)?;
        self.with(py, |d| d.start(timeout))
    }
    fn stop(&self, py: Python<'_>, timeout: f64) -> PyResult<()> {
        let timeout = duration(timeout)?;
        self.with(py, |d| d.stop(timeout))
    }
    fn close(&self, py: Python<'_>, timeout: f64) -> PyResult<()> {
        let timeout = duration(timeout)?;
        self.with(py, |d| d.close(timeout))
    }
    fn configure(&self, py: Python<'_>, config_json: &str, timeout: f64) -> PyResult<()> {
        let timeout = duration(timeout)?;
        let config: Config = serde_json::from_str(config_json)
            .map_err(|e| ConfigurationError::new_err(e.to_string()))?;
        self.with(py, |d| d.configure(config.into(), timeout))
    }
    fn snapshot_json(&self, py: Python<'_>) -> PyResult<String> {
        let s = self.with(py, |d| Ok(d.snapshot()))?;
        let config = s.config.map(|c| serde_json::json!({"reference":c.reference,"mode":c.mode,
            "enabled_mask":c.enabled_mask,"bias_mask":c.bias_mask,"srb2_mask":c.srb2_mask,"gains":c.gains,"verified":c.verified}));
        let metadata = s.metadata.map(|m| serde_json::json!({"firmware":m.firmware,"hardware":m.hardware,"protocol":m.protocol}));
        Ok(serde_json::json!({"state":format!("{:?}",s.state).to_lowercase(),"config":config,"metadata":metadata,
            "generation":s.generation,"samples":s.samples,"missing_samples":s.missing_samples,"crc_errors":s.crc_errors,
            "ble":s.ble,"error":s.error.map(|e| e.to_string())}).to_string())
    }
    fn read<'py>(&self, py: Python<'py>, timeout: f64) -> PyResult<Bound<'py, PyDict>> {
        let timeout = duration(timeout)?;
        let samples = self.with(py, |d| d.read(timeout))?;
        pack_samples(py, samples)
    }
}
impl Drop for NativeDevice {
    fn drop(&mut self) {
        // Forgotten explicit close must not hold Python's GIL during SDK shutdown.
        if let Ok(guard) = self.device.get_mut()
            && let Some(device) = guard.take()
        {
            let _ = std::thread::Builder::new()
                .name("omnibci-python-close".into())
                .spawn(move || drop(device));
        }
    }
}
fn pack_samples<'py>(py: Python<'py>, samples: Samples) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    let n = samples.frames.len();
    let mut uv = Vec::with_capacity(n * 8 * 4);
    let mut counts = Vec::with_capacity(n * 8 * 4);
    let mut sequence = Vec::with_capacity(n * 4);
    let mut valid = Vec::with_capacity(n);
    let mut status = Vec::with_capacity(n * 3);
    let mut mode = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(n * 8);
    for frame in &samples.frames {
        if frame.values_uv.len() != 8
            || frame.raw_counts.as_ref().is_none_or(|v| v.len() != 8)
            || frame.status.len() != 3
        {
            return Err(TransportError::new_err("unexpected ESP32 sample shape"));
        }
        for value in &frame.values_uv {
            uv.extend(value.to_le_bytes());
        }
        for value in frame.raw_counts.as_ref().unwrap() {
            counts.extend(value.to_le_bytes());
        }
        sequence.extend(frame.sequence.to_le_bytes());
        valid.push(u8::from(frame.valid));
        status.extend(&frame.status);
        mode.push(frame.mode);
    }
    for index in samples.sample_indices {
        indices.extend(index.to_le_bytes());
    }
    for (key, value) in [
        ("eeg_uv", uv),
        ("raw_counts", counts),
        ("sequence", sequence),
        ("valid", valid),
        ("status", status),
        ("mode", mode),
        ("sample_indices", indices),
    ] {
        output.set_item(key, PyBytes::new(py, &value))?;
    }
    output.set_item("generation", samples.generation)?;
    output.set_item("sample_rate_hz", samples.sample_rate_hz)?;
    output.set_item(
        "received_age_s",
        samples.received_at.elapsed().as_secs_f64(),
    )?;
    Ok(output)
}
#[pyfunction]
fn serial_ports(py: Python<'_>) -> PyResult<Vec<String>> {
    py.detach(omnibci_sdk::session::serial_ports)
        .map_err(map_error)
}
#[pyfunction]
fn discover_ble_json(py: Python<'_>, timeout: f64) -> PyResult<String> {
    let timeout = duration(timeout)?;
    let devices = py
        .detach(|| omnibci_sdk::session::discover_ble(timeout))
        .map_err(map_error)?;
    Ok(serde_json::Value::Array(devices.into_iter().map(|d|serde_json::json!({"id":d.key,"name":d.name,"address":d.address,"rssi_dbm":d.rssi_dbm})).collect()).to_string())
}
/// Offline decoding delegates to the same SDK parser as live acquisition.
#[pyfunction]
fn decode_frames<'py>(
    py: Python<'py>,
    data: Vec<u8>,
    gains: [u8; 8],
) -> PyResult<Bound<'py, PyDict>> {
    omnibci_sdk::device_control::frontend::validate_gains(&gains)
        .map_err(ConfigurationError::new_err)?;
    let samples = py.detach(|| {
        let mut decoder = omnibci_sdk::acquisition::SampleDecoder {
            scale: std::array::from_fn(|i| omnibci_sdk::lsb_uv(gains[i] as f32)),
            ..Default::default()
        };
        let batch = decoder.feed(&data, false);
        let mut previous = None;
        let mut index = 0u64;
        let mut indices = Vec::new();
        for frame in &batch.frames {
            index += omnibci_sdk::protocol::sequence_gap_size(previous, frame.sequence) as u64;
            indices.push(index);
            index += 1;
            previous = Some(frame.sequence);
        }
        Samples {
            generation: 0,
            frames: batch.frames,
            sample_indices: indices,
            received_at: Instant::now(),
            sample_rate_hz: omnibci_sdk::FS,
        }
    });
    pack_samples(py, samples)
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeDevice>()?;
    m.add_function(wrap_pyfunction!(serial_ports, m)?)?;
    m.add_function(wrap_pyfunction!(discover_ble_json, m)?)?;
    m.add_function(wrap_pyfunction!(decode_frames, m)?)?;
    m.add("SDK_VERSION", omnibci_sdk::SDK_VERSION)?;
    m.add("SDK_REVISION", "86539ffdb70a78a6346c23f17e1b3637dc10cfa4")?;
    m.add("OmniBCIError", m.py().get_type::<OmniBCIError>())?;
    m.add(
        "DeviceTimeoutError",
        m.py().get_type::<DeviceTimeoutError>(),
    )?;
    m.add("InvalidStateError", m.py().get_type::<InvalidStateError>())?;
    m.add(
        "ConfigurationError",
        m.py().get_type::<ConfigurationError>(),
    )?;
    m.add("DisconnectedError", m.py().get_type::<DisconnectedError>())?;
    m.add(
        "BufferOverflowError",
        m.py().get_type::<BufferOverflowError>(),
    )?;
    m.add("TransportError", m.py().get_type::<TransportError>())?;
    Ok(())
}
