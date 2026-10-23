mod trigger;
// Thin CPython adapter; all protocol and device operations remain in omnibci-sdk.
use omnibci_sdk::{
    protocol::FrontendConfig,
    session::{Device, Error, Samples},
};
use pyo3::{
    create_exception,
    exceptions::PyValueError,
    prelude::*,
    types::{PyBytes, PyDict, PyInt},
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
fn component_json(firmware: omnibci_sdk::transport::ComponentFirmware) -> serde_json::Value {
    serde_json::json!({
        "version": firmware.version,
        "revision": firmware.revision,
        "dirty": firmware.dirty,
        "state": firmware.state,
        "protocol": firmware.protocol,
    })
}
pub(crate) fn metadata_json(metadata: omnibci_sdk::transport::DeviceMetadata) -> serde_json::Value {
    serde_json::json!({
        "firmware": metadata.firmware,
        "hardware": metadata.hardware,
        "protocol": metadata.protocol,
        "stm32": metadata.stm32.map(component_json),
        "e73": metadata.e73.map(component_json),
    })
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stm32Config {
    mode: u8,
    enabled_mask: u32,
    bias_mask: u32,
    loff_mask: u32,
    gains: [u8; 32],
}
impl From<Stm32Config> for omnibci_sdk::stm32::Configuration {
    fn from(c: Stm32Config) -> Self {
        Self {
            mode: c.mode,
            enabled: c.enabled_mask,
            bias: c.bias_mask,
            loff: c.loff_mask,
            gains: c.gains,
        }
    }
}
enum Backend {
    Esp32(Device),
    Stm32(stm32_session::Device),
}
impl Backend {
    fn width(&self) -> usize {
        match self {
            Self::Esp32(_) => 8,
            Self::Stm32(_) => 32,
        }
    }
    fn start(&mut self, timeout: Duration) -> Result<(), Error> {
        match self {
            Self::Esp32(d) => d.start(timeout),
            Self::Stm32(d) => d.start(timeout),
        }
    }
    fn stop(&mut self, timeout: Duration) -> Result<(), Error> {
        match self {
            Self::Esp32(d) => d.stop(timeout),
            Self::Stm32(d) => d.stop(timeout),
        }
    }
    fn close(&mut self, timeout: Duration) -> Result<(), Error> {
        match self {
            Self::Esp32(d) => d.close(timeout),
            Self::Stm32(d) => d.close(timeout),
        }
    }
    fn read(&mut self, timeout: Duration) -> Result<Samples, Error> {
        match self {
            Self::Esp32(d) => d.read(timeout),
            Self::Stm32(d) => d.read(timeout),
        }
    }
    fn configure(&mut self, json: &str, timeout: Duration) -> Result<(), Error> {
        match self {
            Self::Esp32(d) => {
                let c: Config =
                    serde_json::from_str(json).map_err(|e| Error::InvalidConfig(e.to_string()))?;
                d.configure(c.into(), timeout)
            }
            Self::Stm32(d) => {
                let c: Stm32Config =
                    serde_json::from_str(json).map_err(|e| Error::InvalidConfig(e.to_string()))?;
                d.configure(c.into(), timeout)
            }
        }
    }
    fn snapshot_json(&self) -> String {
        match self {
            Self::Stm32(d) => d.snapshot_json(),
            Self::Esp32(d) => {
                let s = d.snapshot();
                let config = s.config.map(|c| serde_json::json!({"reference":c.reference,"mode":c.mode,"enabled_mask":c.enabled_mask,"bias_mask":c.bias_mask,"srb2_mask":c.srb2_mask,"gains":c.gains,"verified":c.verified}));
                let metadata = s.metadata.map(metadata_json);
                serde_json::json!({"state":format!("{:?}",s.state).to_lowercase(),"config":config,"metadata":metadata,"generation":s.generation,"samples":s.samples,"missing_samples":s.missing_samples,"crc_errors":s.crc_errors,"ble":s.ble,"error":s.error.map(|e| e.to_string())}).to_string()
            }
        }
    }
}
#[pyclass(frozen, module = "omnibci._native")]
struct NativeDevice {
    device: Mutex<Option<Backend>>,
}
impl NativeDevice {
    fn with<T: Send>(
        &self,
        py: Python<'_>,
        operation: impl FnOnce(&mut Backend) -> Result<T, Error> + Send,
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
    #[pyo3(signature = (endpoint, timeout, device_type="esp32"))]
    fn connect(py: Python<'_>, endpoint: &str, timeout: f64, device_type: &str) -> PyResult<Self> {
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
            .detach(|| match device_type {
                "esp32" => match kind {
                    "serial" => Device::connect_serial(address, timeout),
                    _ => Device::connect_ble(address, timeout),
                }
                .map(Backend::Esp32),
                "stm32" => {
                    stm32_session::Device::connect(kind, address, timeout).map(Backend::Stm32)
                }
                _ => Err(Error::InvalidConfig(
                    "device_type must be esp32 or stm32".into(),
                )),
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
        self.with(py, |d| d.configure(config_json, timeout))
    }
    /// Enable or disable LOFF; success requires a fresh SDK readback confirmation.
    fn set_lead_off(&self, py: Python<'_>, mask: &Bound<'_, PyAny>, timeout: f64) -> PyResult<()> {
        if !mask.is_exact_instance_of::<PyInt>() {
            return Err(PyValueError::new_err("mask must be an integer in [0, 255]"));
        }
        let mask = mask
            .extract::<u8>()
            .map_err(|_| PyValueError::new_err("mask must be an integer in [0, 255]"))?;
        let timeout = duration(timeout)?;
        self.with(py, |d| match d {
            Backend::Esp32(device) => device.set_lead_off(mask, timeout),
            Backend::Stm32(_) => Err(Error::InvalidConfig(
                "use STM32 configuration for LOFF".into(),
            )),
        })
    }
    fn snapshot_json(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, |d| Ok(d.snapshot_json()))
    }
    fn read_triggers_json(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, |d| match d {
            Backend::Stm32(device) => {
                let (generation, events) = device.read_triggers()?;
                Ok(triggers_json(generation, &events).to_string())
            }
            Backend::Esp32(_) => Err(Error::InvalidConfig(
                "hardware trigger polling currently requires STM32".into(),
            )),
        })
    }
    fn read<'py>(&self, py: Python<'py>, timeout: f64) -> PyResult<Bound<'py, PyDict>> {
        let timeout = duration(timeout)?;
        let samples = self.with(py, |d| d.read(timeout))?;
        pack_samples(py, samples, self.with(py, |d| Ok(d.width()))?)
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
fn pack_samples<'py>(
    py: Python<'py>,
    samples: Samples,
    width: usize,
) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    let n = samples.frames.len();
    let status_width = width / 8 * 3;
    let mut uv = Vec::with_capacity(n * width * 4);
    let mut counts = Vec::with_capacity(n * width * 4);
    let mut sequence = Vec::with_capacity(n * 4);
    let mut valid = Vec::with_capacity(n);
    let mut status = Vec::with_capacity(n * status_width);
    let mut mode = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(n * 8);
    for frame in &samples.frames {
        if frame.values_uv.len() != width
            || frame.raw_counts.as_ref().is_none_or(|v| v.len() != width)
            || frame.status.len() != status_width
        {
            return Err(TransportError::new_err("unexpected device sample shape"));
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
    let timestamps: Vec<u8> = samples
        .frames
        .iter()
        .flat_map(|f| f.source_timestamp_us.unwrap_or(0).to_le_bytes())
        .collect();
    let timestamp_valid: Vec<u8> = samples
        .frames
        .iter()
        .map(|f| u8::from(f.source_timestamp_us.is_some()))
        .collect();
    output.set_item("source_timestamp_us", PyBytes::new(py, &timestamps))?;
    output.set_item("source_timestamp_valid", PyBytes::new(py, &timestamp_valid))?;
    output.set_item("channel_count", width)?;
    output.set_item("status_width", status_width)?;
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
#[pyo3(signature = (timeout, device_type="esp32"))]
fn discover_ble_json(py: Python<'_>, timeout: f64, device_type: &str) -> PyResult<String> {
    let timeout = duration(timeout)?;
    let devices = py
        .detach(|| match device_type {
            "esp32" => omnibci_sdk::session::discover_ble(timeout),
            "stm32" => stm32_session::discover(timeout),
            _ => Err(Error::InvalidConfig(
                "device_type must be esp32 or stm32".into(),
            )),
        })
        .map_err(map_error)?;
    Ok(serde_json::Value::Array(devices.into_iter().map(|d|serde_json::json!({"id":d.key,"name":d.name,"address":d.address,"rssi_dbm":d.rssi_dbm})).collect()).to_string())
}
/// Offline decoding delegates to the same SDK parser as live acquisition.
#[pyfunction]
#[pyo3(signature = (data, gains, device_type="esp32"))]
fn decode_frames<'py>(
    py: Python<'py>,
    data: Vec<u8>,
    gains: Vec<u8>,
    device_type: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let width = match device_type {
        "esp32" => 8,
        "stm32" => 32,
        _ => return Err(PyValueError::new_err("device_type must be esp32 or stm32")),
    };
    if gains.len() != width || gains.iter().any(|g| ![1, 2, 4, 6, 8, 12, 24].contains(g)) {
        return Err(ConfigurationError::new_err("invalid gains"));
    }
    let samples = py.detach(|| {
        let frames = if width == 32 {
            let mut parser = omnibci_sdk::stm32::Parser::default();
            parser.feed(&data, gains.as_slice().try_into().unwrap())
        } else {
            let mut decoder = omnibci_sdk::acquisition::SampleDecoder {
                scale: std::array::from_fn(|i| omnibci_sdk::lsb_uv(gains[i] as f32)),
                ..Default::default()
            };
            decoder.feed(&data, false).frames
        };
        let mut previous = None;
        let mut index = 0u64;
        let mut indices = Vec::new();
        for frame in &frames {
            index += omnibci_sdk::protocol::sequence_gap_size(previous, frame.sequence) as u64;
            indices.push(index);
            index += 1;
            previous = Some(frame.sequence);
        }
        Samples {
            generation: 0,
            frames,
            sample_indices: indices,
            received_at: Instant::now(),
            sample_rate_hz: omnibci_sdk::FS,
        }
    });
    pack_samples(py, samples, width)
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeDevice>()?;
    m.add_function(wrap_pyfunction!(decode_triggers_json, m)?)?;
    m.add_class::<trigger::NativeTriggerController>()?;
    m.add_function(wrap_pyfunction!(serial_ports, m)?)?;
    m.add_function(wrap_pyfunction!(discover_ble_json, m)?)?;
    m.add_function(wrap_pyfunction!(decode_frames, m)?)?;
    m.add("SDK_VERSION", omnibci_sdk::SDK_VERSION)?;
    m.add("SDK_REVISION", "6052d32ba71c31411f7c8bfca8fa32f7a600ac0d")?;
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
mod stm32_session;
#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    fn native_stm32_configuration_rejects_untrusted_fields() {
        let valid = serde_json::json!({"mode":0,"enabled_mask":4294967295u32,"bias_mask":4294967295u32,"loff_mask":0,"gains":vec![24;32]});
        let mut invalid = vec![];
        for (key, value) in [
            ("mode", serde_json::json!(5)),
            ("enabled_mask", serde_json::json!(true)),
            ("enabled_mask", serde_json::json!(4294967296u64)),
            ("enabled_mask", serde_json::json!(-1)),
            ("gains", serde_json::json!(vec![24; 8])),
            ("gains", serde_json::json!(vec![3; 32])),
            ("gains", serde_json::json!(vec![true; 32])),
            ("enabled_mask", serde_json::json!(1)),
        ] {
            let mut config = valid.clone();
            config[key] = value;
            invalid.push(config);
        }
        for json in invalid {
            let result = serde_json::from_value::<Stm32Config>(json)
                .map_err(|e| e.to_string())
                .and_then(|config| omnibci_sdk::stm32::Configuration::from(config).validate());
            assert!(result.is_err());
        }
        let config = serde_json::from_value::<Stm32Config>(valid).unwrap();
        assert!(
            omnibci_sdk::stm32::Configuration::from(config)
                .validate()
                .is_ok()
        );
    }
    #[test]
    fn metadata_json_keeps_legacy_fields_and_exposes_components() {
        let metadata = omnibci_sdk::transport::DeviceMetadata {
            stm32: Some(omnibci_sdk::transport::ComponentFirmware {
                version: Some("32.1.0".into()),
                revision: Some("012345abcdef".into()),
                dirty: Some(true),
                state: "available".into(),
                protocol: "EEG V3 · Control V1".into(),
            }),
            e73: None,
            firmware: "STM32 32.1.0".into(),
            hardware: "STM32H563".into(),
            protocol: "EEG V3".into(),
        };
        let value = metadata_json(metadata);
        assert_eq!(value["firmware"], "STM32 32.1.0");
        assert_eq!(value["hardware"], "STM32H563");
        assert_eq!(value["protocol"], "EEG V3");
        assert_eq!(value["stm32"]["version"], "32.1.0");
        assert_eq!(value["stm32"]["revision"], "012345abcdef");
        assert_eq!(value["stm32"]["dirty"].as_bool(), Some(true));
        assert_eq!(value["stm32"]["state"], "available");
        assert_eq!(value["stm32"]["protocol"], "EEG V3 · Control V1");
        assert!(value["e73"].is_null());
    }
}

fn triggers_json(
    generation: u64,
    events: &[omnibci_sdk::protocol::EegTrigger],
) -> serde_json::Value {
    serde_json::json!({"generation": generation, "events": events.iter().map(|event| {
        serde_json::json!({"code":event.code,"sequence":event.sequence,
            "source_timestamp_ms":event.source_timestamp_ms,
            "timing":event.timing.as_ref().map(|timing| serde_json::json!({
                "trigger_timestamp_us": timing.trigger_timestamp_us,
                "anchor_timestamp_us":timing.anchor_timestamp_us
            }))})
    }).collect::<Vec<_>>()})
}

#[pyfunction]
fn decode_triggers_json(data: Vec<u8>) -> String {
    let mut parser = omnibci_sdk::stm32::Parser::default();
    parser.feed(&data, &[24; 32]);
    triggers_json(0, &parser.events).to_string()
}
