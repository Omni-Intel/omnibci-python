//! Synchronous STM32 owner. The SDK alone owns wire transactions and radio framing.
use crossbeam_channel::{SendTimeoutError, Sender, bounded};
use omnibci_sdk::{
    session::{Error, Samples, State},
    stm32::{Configuration, Parser},
    transport::{Command, DeliveryError, DeviceInfo, DeviceMetadata, Event, EventSink},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Error>;
const SAMPLE_CAPACITY: usize = 128;
#[derive(Clone)]
enum Action {
    Start,
    Stop,
    Configure(Configuration),
}
struct Core {
    state: State,
    config: Option<Configuration>,
    metadata: Option<DeviceMetadata>,
    error: Option<Error>,
    cleanup_stop_error: Option<Error>,
    pending: Option<Action>,
    control_deadline: Option<Instant>,
    connected: bool,
    generation: u64,
    samples: u64,
    missing: u64,
    crc: u64,
    parser: Parser,
    previous: Option<u32>,
    index: u64,
    queue: VecDeque<Samples>,
    disconnected: bool,
    closing: bool,
    loff_may_be_on: bool,
}
impl Default for Core {
    fn default() -> Self {
        Self {
            state: State::Connecting,
            config: None,
            metadata: None,
            error: None,
            cleanup_stop_error: None,
            pending: None,
            control_deadline: None,
            connected: false,
            generation: 0,
            samples: 0,
            missing: 0,
            crc: 0,
            parser: Parser::default(),
            previous: None,
            index: 0,
            queue: VecDeque::new(),
            disconnected: false,
            closing: false,
            loff_may_be_on: false,
        }
    }
}
impl Core {
    fn fault(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        self.state = State::Faulted;
        self.pending = None;
        self.control_deadline = None;
    }
    /// Keep the first distinct cleanup report for close diagnostics. The
    /// primary fault remains in `error`; an identical report is not repeated.
    fn record_cleanup(&mut self, error: Error) {
        if self.cleanup_stop_error.is_none() && self.error.as_ref() != Some(&error) {
            self.cleanup_stop_error = Some(error);
        }
    }
    fn begin(&mut self, action: Action) -> Result<Command> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if self.pending.is_some() {
            return Err(Error::InvalidState);
        }
        let command = match &action {
            Action::Start if self.state == State::Ready && self.config.is_some() => {
                self.state = State::Starting;
                Command::Write(b"b".to_vec())
            }
            Action::Stop if matches!(self.state, State::Ready | State::Streaming) => {
                self.state = State::Stopping;
                Command::StopAndDrain
            }
            Action::Configure(config) if self.state == State::Ready => {
                config.validate().map_err(Error::InvalidConfig)?;
                self.loff_may_be_on |= config.loff != 0;
                Command::ConfigureStm32(config.clone())
            }
            _ => return Err(Error::InvalidState),
        };
        self.pending = Some(action);
        Ok(command)
    }
    fn event(&mut self, event: Event) -> Result<()> {
        if self
            .control_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            && (self.pending.is_some() || self.state == State::Connecting)
        {
            self.fault(Error::Timeout);
        }
        // A timeout invalidates the control generation. No late ACK can revive it.
        if self.error.is_some() {
            match event {
                Event::Disconnected => self.disconnected = true,
                // Cleanup diagnostics survive an earlier fault without changing
                // state or masking the primary error.
                Event::Stm32Stopped(Err(error)) if self.closing => {
                    self.record_cleanup(Error::Transport(error));
                }
                Event::Error(message) if self.closing => {
                    self.record_cleanup(Error::Transport(message));
                }
                _ => {}
            }
            return Ok(());
        }
        match event {
            Event::Connected(_) => {
                self.connected = true;
            }
            Event::DeviceMetadata(metadata) => self.metadata = Some(metadata),
            Event::Stm32Configured(result) => {
                let config = result.map_err(Error::InvalidConfig)?;
                config.validate().map_err(Error::InvalidConfig)?;
                match &self.pending {
                    Some(Action::Configure(expected)) if expected == &config => self.pending = None,
                    Some(_) => {
                        return Err(Error::InvalidConfig(
                            "STM32 configuration ACK does not match request".into(),
                        ));
                    }
                    None if self.state == State::Connecting
                        && config == Configuration::default() => {}
                    _ => {
                        return Err(Error::InvalidConfig(
                            "unexpected STM32 configuration ACK".into(),
                        ));
                    }
                }
                self.loff_may_be_on = config.loff != 0;
                self.config = Some(config);
            }
            Event::Stm32Started(result) if matches!(self.pending, Some(Action::Start)) => {
                result.map_err(Error::Transport)?;
                self.queue.clear();
                self.parser = Parser::default();
                self.previous = None;
                self.index = 0;
                self.samples = 0;
                self.missing = 0;
                self.generation += 1;
                self.state = State::Streaming;
                self.pending = None;
            }
            Event::Stm32Stopped(result) if matches!(self.pending, Some(Action::Stop)) => {
                result.map_err(Error::Transport)?;
                self.state = State::Ready;
                self.pending = None;
            }
            // A failed Disconnect stop leaves the latest SDK driver running until
            // Shutdown. Its best-effort cleanup report must not vanish during close.
            Event::Stm32Stopped(result) if self.pending.is_none() && self.closing => {
                if let Err(error) = result {
                    self.record_cleanup(Error::Transport(error));
                }
            }
            Event::Bytes(bytes)
                if matches!(
                    self.state,
                    State::Streaming | State::Stopping | State::Starting
                ) =>
            {
                let config = self.config.as_ref().ok_or(Error::InvalidState)?;
                let frames = self.parser.feed(&bytes, &config.gains);
                self.parser.data_packets.clear();
                self.parser.events.clear();
                if !frames.is_empty() {
                    if self.queue.len() == SAMPLE_CAPACITY {
                        return Err(Error::Overflow);
                    }
                    let mut indices = Vec::with_capacity(frames.len());
                    for frame in &frames {
                        let gap =
                            omnibci_sdk::protocol::sequence_gap_size(self.previous, frame.sequence)
                                as u64;
                        self.missing += gap;
                        self.index += gap;
                        indices.push(self.index);
                        self.index += 1;
                        self.previous = Some(frame.sequence);
                    }
                    self.samples += frames.len() as u64;
                    self.queue.push_back(Samples {
                        generation: self.generation,
                        frames,
                        sample_indices: indices,
                        received_at: Instant::now(),
                        sample_rate_hz: 250,
                    });
                }
            }
            Event::Stm32WireMetrics { crc_bad, .. } => self.crc = crc_bad,
            Event::Error(message) => return Err(Error::Transport(message)),
            Event::Disconnected => {
                self.disconnected = true;
                if self.closing {
                    self.state = State::Closed;
                } else {
                    self.fault(Error::Disconnected);
                }
            }
            _ => {}
        }
        if self.state == State::Connecting && self.connected && self.config.is_some() {
            self.state = State::Ready;
        }
        if self.pending.is_none() && self.state != State::Connecting {
            self.control_deadline = None;
        }
        Ok(())
    }
}
struct Shared {
    core: Mutex<Core>,
    changed: Condvar,
}
pub struct Device {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    shutdown: Sender<()>,
    worker: Option<JoinHandle<()>>,
    driver: Option<JoinHandle<()>>,
}
impl Device {
    pub fn connect(kind: &str, address: &str, timeout: Duration) -> Result<Self> {
        Self::connect_with(
            timeout,
            |commands, sink| {
                if kind == "serial" {
                    omnibci_sdk::stm32::spawn_into(
                        address.into(),
                        Configuration::default(),
                        commands,
                        sink,
                    )
                } else {
                    omnibci_sdk::e73_ble::spawn_into(Configuration::default(), commands, sink)
                }
            },
            (kind == "ble").then(|| address.to_string()),
        )
    }
    fn connect_with(
        timeout: Duration,
        spawn: impl FnOnce(crossbeam_channel::Receiver<Command>, EventSink) -> JoinHandle<()>,
        key: Option<String>,
    ) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        let (commands, command_rx) = bounded(32);
        let (events, event_rx) = bounded(1024);
        let overflow = Arc::new(AtomicBool::new(false));
        let flag = overflow.clone();
        let sink = EventSink::new(move |event| {
            events.try_send(event).map_err(|e| match e {
                crossbeam_channel::TrySendError::Full(_) => {
                    flag.store(true, Ordering::Release);
                    DeliveryError::Full
                }
                _ => DeliveryError::Closed,
            })
        });
        let driver = spawn(command_rx, sink);
        let shared = Arc::new(Shared {
            core: Mutex::new(Core {
                control_deadline: Some(deadline),
                ..Default::default()
            }),
            changed: Condvar::new(),
        });
        let state = shared.clone();
        let (shutdown, quit) = bounded(1);
        let control = commands.clone();
        let worker = thread::spawn(move || {
            loop {
                if quit.try_recv().is_ok() {
                    break;
                }
                let event = event_rx.recv_timeout(Duration::from_millis(10));
                let mut core = state.core.lock().unwrap();
                if overflow.load(Ordering::Acquire) {
                    core.fault(Error::Overflow);
                }
                match event {
                    Ok(event) => {
                        if let Err(error) = core.event(event) {
                            core.fault(error);
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        core.disconnected = true;
                        core.fault(Error::Disconnected);
                    }
                    _ => {}
                }
                // Retry fault cleanup without filling the bounded queue: keep at
                // most one request pending and add none while close owns cleanup.
                if core.error.is_some() && !core.closing && control.is_empty() {
                    let _ = control.try_send(Command::Disconnect);
                }
                state.changed.notify_all();
                if core.disconnected {
                    break;
                }
            }
        });
        let mut device = Self {
            shared,
            commands,
            shutdown,
            worker: Some(worker),
            driver: Some(driver),
        };
        if let Some(key) = key {
            device.send(Command::Scan)?;
            device.send(Command::Connect(key))?;
        }
        if let Err(error) = device.wait(deadline, |core| core.state == State::Ready) {
            let _ = device.close(Duration::from_millis(100));
            return Err(error);
        }
        Ok(device)
    }
    fn send(&self, command: Command) -> Result<()> {
        self.commands.try_send(command).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => Error::Overflow,
            _ => Error::Disconnected,
        })
    }
    /// Control commands used by cleanup must not be dropped when the bounded
    /// queue is momentarily full. Always try to enqueue immediately; only
    /// waiting for a full queue consumes the caller's budget.
    fn send_before(&self, command: Command, deadline: Instant) -> Result<()> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => Err(Error::Disconnected),
            Err(crossbeam_channel::TrySendError::Full(command)) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::Timeout);
                }
                self.commands
                    .send_timeout(command, remaining)
                    .map_err(|error| match error {
                        SendTimeoutError::Timeout(_) => Error::Timeout,
                        SendTimeoutError::Disconnected(_) => Error::Disconnected,
                    })
            }
        }
    }
    fn wait(&self, deadline: Instant, ready: impl Fn(&Core) -> bool) -> Result<()> {
        let mut core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
        loop {
            if let Some(error) = &core.error {
                return Err(error.clone());
            }
            if ready(&core) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                core.fault(Error::Timeout);
                self.shared.changed.notify_all();
                let _ = self.send(Command::Disconnect);
                return Err(Error::Timeout);
            }
            core = self
                .shared
                .changed
                .wait_timeout(core, remaining)
                .map_err(|_| Error::WorkerPanicked)?
                .0;
        }
    }
    fn request(&self, action: Action, timeout: Duration) -> Result<()> {
        if timeout.is_zero() {
            return Err(Error::Timeout);
        }
        let deadline = Instant::now() + timeout;
        let command = {
            let mut core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
            let command = core.begin(action)?;
            core.control_deadline = Some(deadline);
            command
        };
        if let Err(error) = self.send(command) {
            self.shared.core.lock().unwrap().fault(error.clone());
            return Err(error);
        }
        self.wait(deadline, |core| core.pending.is_none())
    }
    pub fn start(&mut self, timeout: Duration) -> Result<()> {
        self.request(Action::Start, timeout)
    }
    pub fn stop(&mut self, timeout: Duration) -> Result<()> {
        self.request(Action::Stop, timeout)
    }
    pub fn configure(&mut self, config: Configuration, timeout: Duration) -> Result<()> {
        config.validate().map_err(Error::InvalidConfig)?;
        self.request(Action::Configure(config), timeout)
    }
    pub fn read(&self, timeout: Duration) -> Result<Samples> {
        let deadline = Instant::now() + timeout;
        let mut core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
        loop {
            // Deliver the retained prefix before reporting an asynchronous failure.
            if let Some(batch) = core.queue.pop_front() {
                return Ok(batch);
            }
            if let Some(error) = &core.error {
                return Err(error.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            core = self
                .shared
                .changed
                .wait_timeout(core, remaining)
                .map_err(|_| Error::WorkerPanicked)?
                .0;
        }
    }
    pub fn snapshot_json(&self) -> String {
        let core = self.shared.core.lock().unwrap();
        let config = core.config.as_ref().map(|c| serde_json::json!({"mode":c.mode,"enabled_mask":c.enabled,"bias_mask":c.bias,"loff_mask":c.loff,"gains":c.gains.as_slice(),"verified":true}));
        let metadata = core
            .metadata
            .as_ref()
            .map(|m| crate::metadata_json(m.clone()));
        serde_json::json!({"state":format!("{:?}",core.state).to_lowercase(),"config":config,"metadata":metadata,"generation":core.generation,"samples":core.samples,"missing_samples":core.missing,"crc_errors":core.crc,"ble":{},"error":core.error.as_ref().map(ToString::to_string)}).to_string()
    }
    pub fn close(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut result = Ok(());
        let mut cleanup_errors = Vec::new();
        let (state, config, original) = {
            let core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
            (core.state, core.config.clone(), core.error.clone())
        };
        if original.is_none() && matches!(state, State::Streaming | State::Ready) {
            result = self.stop(deadline.saturating_duration_since(Instant::now()));
            if result.is_ok()
                && let Some(mut config) = config
                && config.loff != 0
            {
                config.loff = 0;
                result = self.configure(config, deadline.saturating_duration_since(Instant::now()));
            }
        }
        self.shared
            .core
            .lock()
            .map_err(|_| Error::WorkerPanicked)?
            .closing = true;
        match self.send_before(Command::Disconnect, deadline) {
            Ok(()) | Err(Error::Disconnected) => {}
            Err(error) => cleanup_errors.push(error),
        }
        // The latest SDK keeps the driver alive when Disconnect's stop fails.
        // Allow the normal stop-and-exit path a moment, then request Shutdown,
        // whose wrapper performs a best-effort stop before the thread exits.
        // With no remaining budget this waits zero time; send_before still
        // attempts the enqueue immediately when queue capacity is available.
        let escalate = (Instant::now() + Duration::from_millis(100)).min(deadline);
        while self.driver.as_ref().is_some_and(|h| !h.is_finished()) && Instant::now() < escalate {
            thread::sleep(Duration::from_millis(2));
        }
        if self.driver.as_ref().is_some_and(|h| !h.is_finished()) {
            match self.send_before(Command::Shutdown, deadline) {
                Ok(()) | Err(Error::Disconnected) => {}
                Err(error) if !cleanup_errors.contains(&error) => cleanup_errors.push(error),
                Err(_) => {}
            }
        }
        // ponytail: OS drivers cannot be forcibly cancelled; detach on deadline, never join a live worker.
        while self.driver.as_ref().is_some_and(|h| !h.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        if let Some(driver) = self.driver.take() {
            if driver.is_finished() {
                if driver.join().is_err() {
                    cleanup_errors.push(Error::WorkerPanicked);
                }
            } else {
                cleanup_errors.push(Error::Timeout);
            }
        }
        // The driver finishing does not mean its final error events were consumed.
        while !self
            .shared
            .core
            .lock()
            .map_err(|_| Error::WorkerPanicked)?
            .disconnected
            && self.worker.as_ref().is_some_and(|h| !h.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        // A full slot means quit was already queued; Disconnected means the
        // worker has exited. Neither is a cleanup failure.
        let _ = self.shutdown.try_send(());
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if worker.is_finished() {
                if worker.join().is_err() {
                    cleanup_errors.push(Error::WorkerPanicked);
                }
            } else {
                cleanup_errors.push(Error::Timeout);
            }
        }
        let mut core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
        if original.is_none()
            && let Some(error) = &core.error
        {
            result = match result {
                Ok(()) => Err(error.clone()),
                Err(cleanup) if &cleanup == error => Err(cleanup),
                Err(cleanup) => Err(Error::Transport(format!("{error}; cleanup: {cleanup}"))),
            };
        }
        for cleanup in cleanup_errors {
            result = match result {
                Ok(()) => Err(cleanup),
                Err(primary) => Err(Error::Transport(format!("{primary}; cleanup: {cleanup}"))),
            };
        }
        let cleanup_stop = core.cleanup_stop_error.take();
        if let Some(error) = cleanup_stop
            && core.error.as_ref() != Some(&error)
        {
            result = match result {
                Ok(()) => Err(error),
                Err(primary) => Err(Error::Transport(format!("{primary}; cleanup: {error}"))),
            };
        }
        if core.loff_may_be_on {
            let detail = "LOFF excitation off could not be confirmed after session failure";
            result = Err(Error::Transport(match result {
                Ok(()) => detail.into(),
                Err(error) => format!("{error}; {detail}"),
            }));
        }
        if let Err(error) = &result {
            core.fault(error.clone());
        }
        if original.is_none() && result.is_ok() {
            core.state = State::Closed;
        }
        if let Some(error) = original {
            return match result {
                Ok(()) => Err(error),
                Err(cleanup) => Err(Error::Transport(format!("{error}; cleanup: {cleanup}"))),
            };
        }
        result
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.close(Duration::from_secs(6));
        }
    }
}
pub fn discover(timeout: Duration) -> Result<Vec<DeviceInfo>> {
    let deadline = Instant::now() + timeout;
    let (command, commands) = bounded::<Command>(8);
    let (events, receiver) = bounded(64);
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = overflow.clone();
    let sink = EventSink::new(move |event| {
        events.try_send(event).map_err(|_| {
            flag.store(true, Ordering::Release);
            DeliveryError::Full
        })
    });
    let worker = omnibci_sdk::e73_ble::spawn_into(Configuration::default(), commands, sink);
    command
        .try_send(Command::Scan)
        .map_err(|_| Error::Disconnected)?;
    let result = loop {
        if overflow.load(Ordering::Acquire) {
            break Err(Error::Overflow);
        }
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Event::Devices(devices)) => break Ok(devices),
            Ok(Event::Error(error)) => break Err(Error::Transport(error)),
            Ok(Event::Disconnected) => break Err(Error::Disconnected),
            Err(_) => break Err(Error::Timeout),
            _ => {}
        }
    };
    let _ = command.try_send(Command::Shutdown);
    // Scan cancellation is owned by the SDK; never exceed the caller's deadline to join it.
    if worker.is_finished() {
        worker.join().map_err(|_| Error::WorkerPanicked)?;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_requires_ack_and_timeout_rejects_late_ack() {
        let mut state = Core::default();
        state.config = Some(Configuration::default());
        state.state = State::Ready;
        state.begin(Action::Start).unwrap();
        assert_eq!(state.state, State::Starting);
        state.event(Event::Stm32Started(Ok(()))).unwrap();
        assert_eq!(state.state, State::Streaming);
        state.fault(Error::Timeout);
        state.event(Event::Stm32Started(Ok(()))).unwrap();
        assert_eq!(state.state, State::Faulted);
    }
    #[test]
    fn config_requires_matching_ack() {
        let mut state = Core::default();
        state.state = State::Ready;
        let wanted = Configuration {
            gains: [12; 32],
            ..Default::default()
        };
        state.begin(Action::Configure(wanted)).unwrap();
        assert!(matches!(
            state.event(Event::Stm32Configured(Ok(Configuration::default()))),
            Err(Error::InvalidConfig(_))
        ));
    }
    #[test]
    fn disconnect_faults_generation() {
        let mut state = Core::default();
        state.state = State::Streaming;
        state.event(Event::Disconnected).unwrap();
        assert_eq!(state.state, State::Faulted);
        assert_eq!(state.error, Some(Error::Disconnected));
    }
}
#[cfg(test)]
mod integration_tests {
    use super::*;
    fn simulated(
        mut handle: impl FnMut(Command, &EventSink) + Send + 'static,
    ) -> (Device, EventSink) {
        let (sink_tx, sink_rx) = bounded(1);
        let device = Device::connect_with(
            Duration::from_secs(1),
            move |commands, events| {
                sink_tx.send(events.clone()).unwrap();
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        if matches!(command, Command::Disconnect | Command::Shutdown) {
                            let _ = events.send(Event::Disconnected);
                            break;
                        }
                        handle(command, &events);
                    }
                })
            },
            None,
        )
        .unwrap();
        (device, sink_rx.recv().unwrap())
    }
    fn packet(sequence: u32) -> Vec<u8> {
        let mut packet = vec![0; 128];
        packet[..4].copy_from_slice(&[0xa5, 0x5a, 3, 1]);
        packet[4..8].copy_from_slice(&sequence.to_le_bytes());
        for i in [12, 15, 18, 21] {
            packet[i] = 0xc0;
        }
        packet[24] = 3;
        packet[125] = 4;
        let crc = omnibci_sdk::protocol::crc16_ccitt(&packet[..126]);
        packet[126..].copy_from_slice(&crc.to_le_bytes());
        packet
    }
    fn acknowledge(command: Command, events: &EventSink) {
        match command {
            Command::Write(_) => events.send(Event::Stm32Started(Ok(()))).unwrap(),
            Command::StopAndDrain => events.send(Event::Stm32Stopped(Ok(()))).unwrap(),
            Command::ConfigureStm32(config) => {
                events.send(Event::Stm32Configured(Ok(config))).unwrap()
            }
            _ => {}
        }
    }
    #[test]
    fn background_start_publishes_generation_only_after_ack() {
        let (seen, receiver) = bounded(1);
        let (mut device, events) = simulated(move |_, _| {
            seen.send(()).unwrap();
        });
        let shared = device.shared.clone();
        let starting = thread::spawn(move || {
            device.start(Duration::from_secs(1)).unwrap();
            device
        });
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let generation = shared.core.lock().unwrap().generation;
        events.send(Event::Stm32Started(Ok(()))).unwrap();
        let mut device = starting.join().unwrap();
        assert_eq!(generation, 0, "START submission is not hardware success");
        assert_eq!(device.shared.core.lock().unwrap().generation, 1);
        // No STOP acknowledgement in this transport: close must be bounded and fail.
        assert!(device.close(Duration::from_millis(30)).is_err());
    }
    #[test]
    fn background_tail_survives_stop_and_restart_clears_old_samples() {
        let (mut device, events) = simulated(|command, events| {
            if matches!(command, Command::StopAndDrain) {
                events.send(Event::Bytes(packet(4))).unwrap();
            }
            acknowledge(command, events);
        });
        device.start(Duration::from_secs(1)).unwrap();
        events.send(Event::Bytes(packet(u32::MAX))).unwrap();
        events.send(Event::Bytes(packet(0))).unwrap();
        device.stop(Duration::from_secs(1)).unwrap();
        assert_eq!(
            device.read(Duration::from_secs(1)).unwrap().sample_indices,
            [0]
        );
        assert_eq!(
            device.read(Duration::from_secs(1)).unwrap().sample_indices,
            [1]
        );
        assert_eq!(
            device.read(Duration::from_secs(1)).unwrap().sample_indices,
            [5]
        );
        device.start(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            device.read(Duration::from_millis(5)),
            Err(Error::Timeout)
        ));
        device.close(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn idle_consumer_overflow_preserves_prefix_then_reports_error() {
        let (mut device, events) = simulated(acknowledge);
        device.start(Duration::from_secs(1)).unwrap();
        for sequence in 0..=SAMPLE_CAPACITY {
            events.send(Event::Bytes(packet(sequence as u32))).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while device.shared.core.lock().unwrap().error.is_none() && Instant::now() < deadline {
            thread::yield_now();
        }
        for sequence in 0..SAMPLE_CAPACITY {
            assert_eq!(
                device.read(Duration::from_secs(1)).unwrap().frames[0].sequence,
                sequence as u32
            );
        }
        assert!(matches!(
            device.read(Duration::from_millis(5)),
            Err(Error::Overflow)
        ));
    }
    #[test]
    fn timeout_faults_and_late_ack_cannot_enable_start() {
        let (mut device, events) = simulated(|_, _| {});
        assert_eq!(device.start(Duration::from_millis(5)), Err(Error::Timeout));
        let _ = events.send(Event::Stm32Started(Ok(())));
        assert_eq!(device.shared.core.lock().unwrap().state, State::Faulted);
        assert_eq!(device.start(Duration::from_millis(5)), Err(Error::Timeout));
    }
    #[test]
    fn loff_cleanup_is_confirmed_and_failed_cleanup_is_reported() {
        for fail in [false, true] {
            let (mut device, _) = simulated(move |command, events| {
                if let Command::ConfigureStm32(config) = &command
                    && config.loff == 0
                    && fail
                {
                    events
                        .send(Event::Stm32Configured(Err("LOFF off rejected".into())))
                        .unwrap();
                } else {
                    acknowledge(command, events);
                }
            });
            device
                .configure(
                    Configuration {
                        loff: 1,
                        ..Default::default()
                    },
                    Duration::from_secs(1),
                )
                .unwrap();
            let result = device.close(Duration::from_secs(1));
            assert_eq!(result.is_err(), fail);
            if !fail {
                assert_eq!(
                    device
                        .shared
                        .core
                        .lock()
                        .unwrap()
                        .config
                        .as_ref()
                        .unwrap()
                        .loff,
                    0
                );
            }
        }
    }
    #[test]
    fn disconnected_generation_retains_received_samples() {
        let (mut device, events) = simulated(acknowledge);
        device.start(Duration::from_secs(1)).unwrap();
        events.send(Event::Bytes(packet(1))).unwrap();
        events.send(Event::Disconnected).unwrap();
        assert_eq!(
            device.read(Duration::from_secs(1)).unwrap().frames[0].sequence,
            1
        );
        assert_eq!(
            device.read(Duration::from_secs(1)).unwrap_err(),
            Error::Disconnected
        );
    }
}
#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn expired_ack_cannot_win_against_waiting_thread() {
        let mut core = Core {
            state: State::Ready,
            config: Some(Configuration::default()),
            ..Default::default()
        };
        core.begin(Action::Start).unwrap();
        core.control_deadline = Some(Instant::now() - Duration::from_millis(1));
        core.event(Event::Stm32Started(Ok(()))).unwrap();
        assert_eq!(core.state, State::Faulted);
        assert_eq!(core.error, Some(Error::Timeout));
        assert_eq!(core.generation, 0);
    }
}
#[cfg(test)]
mod drop_tests {
    use super::*;
    #[test]
    fn forgotten_close_attempts_confirmed_loff_off() {
        let (off, receiver) = bounded(1);
        let (sink, _) = bounded::<EventSink>(1);
        let mut device = Device::connect_with(
            Duration::from_secs(1),
            move |commands, events| {
                let _ = sink.try_send(events.clone());
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        match command {
                            Command::ConfigureStm32(config) => {
                                if config.loff == 0 {
                                    off.send(()).unwrap();
                                }
                                events.send(Event::Stm32Configured(Ok(config))).unwrap();
                            }
                            Command::StopAndDrain => {
                                events.send(Event::Stm32Stopped(Ok(()))).unwrap()
                            }
                            Command::Disconnect | Command::Shutdown => {
                                let _ = events.send(Event::Disconnected);
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        device
            .configure(
                Configuration {
                    loff: 1,
                    ..Default::default()
                },
                Duration::from_secs(1),
            )
            .unwrap();
        drop(device);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("drop must disable LOFF excitation");
    }
}
#[cfg(test)]
mod cleanup_error_tests {
    use super::*;
    #[test]
    fn disconnect_cleanup_error_is_not_cleared_by_closed_state() {
        let mut device = Device::connect_with(
            Duration::from_secs(1),
            move |commands, events| {
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        match command {
                            Command::StopAndDrain => {
                                events.send(Event::Stm32Stopped(Ok(()))).unwrap()
                            }
                            Command::Disconnect => {
                                events
                                    .send(Event::Error("disconnect cleanup failed".into()))
                                    .unwrap();
                                events.send(Event::Disconnected).unwrap();
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        assert!(
            matches!(device.close(Duration::from_secs(1)), Err(Error::Transport(message)) if message.contains("disconnect cleanup failed"))
        );
    }
}
#[cfg(test)]
mod uncertain_loff_tests {
    use super::*;
    #[test]
    fn lost_config_ack_reports_unknown_excitation_even_if_previous_config_was_off() {
        let mut device = Device::connect_with(
            Duration::from_secs(1),
            |commands, events| {
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        if matches!(command, Command::Disconnect) {
                            let _ = events.send(Event::Disconnected);
                            break;
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        assert_eq!(
            device.configure(
                Configuration {
                    loff: 1,
                    ..Default::default()
                },
                Duration::from_millis(5)
            ),
            Err(Error::Timeout)
        );
        let error = device.close(Duration::from_secs(1)).unwrap_err();
        assert!(error.to_string().contains("LOFF"), "{error}");
    }
}
#[cfg(test)]
mod stop_loff_tests {
    use super::*;
    #[test]
    fn failed_close_stop_reports_unknown_loff() {
        let mut device = Device::connect_with(
            Duration::from_secs(1),
            |commands, events| {
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        match command {
                            Command::ConfigureStm32(config) => {
                                events.send(Event::Stm32Configured(Ok(config))).unwrap()
                            }
                            Command::StopAndDrain => events
                                .send(Event::Stm32Stopped(Err("SD close failed".into())))
                                .unwrap(),
                            Command::Disconnect => {
                                let _ = events.send(Event::Disconnected);
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        device
            .configure(
                Configuration {
                    loff: 1,
                    ..Default::default()
                },
                Duration::from_secs(1),
            )
            .unwrap();
        let error = device.close(Duration::from_secs(1)).unwrap_err();
        assert!(error.to_string().contains("LOFF"), "{error}");
        assert!(error.to_string().contains("SD close failed"), "{error}");
    }
}
#[cfg(test)]
mod close_escalation_tests {
    use super::*;

    fn simulated_close(
        explicit_stop: std::result::Result<(), String>,
        disconnect_stop: std::result::Result<(), String>,
        shutdown_stop: std::result::Result<(), String>,
    ) -> (Device, std::sync::mpsc::Receiver<()>) {
        let (shutdown, seen) = std::sync::mpsc::channel();
        let device = Device::connect_with(
            Duration::from_secs(1),
            move |commands, events| {
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        match command {
                            Command::StopAndDrain => {
                                events
                                    .send(Event::Stm32Stopped(explicit_stop.clone()))
                                    .unwrap();
                            }
                            // The latest SDK leaves the driver running when
                            // Disconnect's stop fails.
                            Command::Disconnect => {
                                events
                                    .send(Event::Stm32Stopped(disconnect_stop.clone()))
                                    .unwrap();
                            }
                            Command::Shutdown => {
                                events
                                    .send(Event::Stm32Stopped(shutdown_stop.clone()))
                                    .unwrap();
                                let _ = events.send(Event::Disconnected);
                                let _ = shutdown.send(());
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        (device, seen)
    }

    #[test]
    fn failed_disconnect_stop_escalates_to_shutdown() {
        let (mut device, seen) = simulated_close(
            Err("explicit stop failed".into()),
            Err("disconnect stop failed".into()),
            Err("shutdown cleanup failed".into()),
        );
        let error = device
            .close(Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        seen.recv_timeout(Duration::from_secs(1))
            .expect("close must escalate from Disconnect to Shutdown");
        assert!(error.contains("explicit stop failed"), "{error}");
        assert!(error.contains("disconnect stop failed"), "{error}");
    }

    #[test]
    fn failed_disconnect_stop_is_reported_after_clean_stop() {
        let (mut device, seen) = simulated_close(
            Ok(()),
            Err("disconnect stop failed".into()),
            Err("shutdown cleanup failed".into()),
        );
        let error = device
            .close(Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        seen.recv_timeout(Duration::from_secs(1))
            .expect("close must reject a failed driver cleanup");
        assert!(error.contains("disconnect stop failed"), "{error}");
    }
}
#[cfg(test)]
mod primary_cleanup_error_tests {
    use super::*;
    #[test]
    fn shutdown_timeout_retains_failed_stop_reason() {
        let mut device = Device::connect_with(
            Duration::from_secs(1),
            |commands, events| {
                thread::spawn(move || {
                    events.send(Event::Connected("simulated".into())).unwrap();
                    events
                        .send(Event::Stm32Configured(Ok(Configuration::default())))
                        .unwrap();
                    while let Ok(command) = commands.recv() {
                        match command {
                            Command::StopAndDrain => {
                                events
                                    .send(Event::Stm32Stopped(Err("SD close failed".into())))
                                    .unwrap();
                                thread::sleep(Duration::from_millis(100));
                            }
                            Command::Disconnect => {
                                let _ = events.send(Event::Disconnected);
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            },
            None,
        )
        .unwrap();
        let before = Instant::now();
        let error = device
            .close(Duration::from_millis(10))
            .unwrap_err()
            .to_string();
        assert!(before.elapsed() < Duration::from_millis(80));
        assert!(error.contains("SD close failed"), "{error}");
        assert!(error.contains("Timeout"), "{error}");
    }
}
