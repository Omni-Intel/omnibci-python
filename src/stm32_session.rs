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
const TRIGGER_CAPACITY: usize = 16_384;
/// Upper bound on post-fault LOFF cleanup. Acquisition is stopped immediately;
/// the link is released even if the device never confirms the disable.
const LOFF_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
#[derive(Clone)]
enum Action {
    Start,
    Stop,
    Configure(Configuration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupPhase {
    Stopping,
    Disabling,
    Done,
    Failed,
}

/// Bounded post-fault cleanup of excitation that may still be enabled. It runs
/// after the primary fault, so it never clears that fault or revives the session.
#[derive(Clone)]
struct Cleanup {
    phase: CleanupPhase,
    stop_sent: bool,
    disable: Option<Configuration>,
    disable_sent: bool,
    deadline: Instant,
    failure: Option<Error>,
}

impl Cleanup {
    fn new(now: Instant) -> Self {
        Self {
            phase: CleanupPhase::Stopping,
            stop_sent: false,
            disable: None,
            disable_sent: false,
            deadline: now + LOFF_CLEANUP_TIMEOUT,
            failure: None,
        }
    }
    fn fail(&mut self, error: Error) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.phase = CleanupPhase::Failed;
    }
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
    triggers: VecDeque<omnibci_sdk::protocol::EegTrigger>,
    disconnected: bool,
    closing: bool,
    loff_may_be_on: bool,
    cleanup: Option<Cleanup>,
    stale_stop_acks: usize,
    stale_configure_acks: usize,
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
            triggers: VecDeque::new(),
            disconnected: false,
            closing: false,
            loff_may_be_on: false,
            cleanup: None,
            stale_stop_acks: 0,
            stale_configure_acks: 0,
        }
    }
}
impl Core {
    fn fault(&mut self, error: Error) {
        if self.error.is_none() {
            // ACKs for controls still in flight when the fault occurs belong to
            // the failed generation and cannot confirm post-fault cleanup.
            match &self.pending {
                Some(Action::Stop) => self.stale_stop_acks += 1,
                Some(Action::Configure(_)) => self.stale_configure_acks += 1,
                _ => {}
            }
            self.error = Some(error);
        }
        self.state = State::Faulted;
        self.pending = None;
        self.control_deadline = None;
    }

    fn cleanup_phase(&self) -> Option<CleanupPhase> {
        self.cleanup.as_ref().map(|cleanup| cleanup.phase)
    }

    /// Start the bounded LOFF cleanup at most once per faulted session. A fault
    /// that may have left excitation on must stop acquisition and disable LOFF
    /// before the transport is released.
    fn start_cleanup(&mut self, now: Instant) {
        if self.error.is_some()
            && self.loff_may_be_on
            && !self.disconnected
            && self.cleanup.is_none()
        {
            self.cleanup = Some(Cleanup::new(now));
        }
    }

    /// Next cleanup command to submit. Marking it in flight keeps the worker
    /// and close from issuing the same cleanup step twice.
    fn cleanup_command(&mut self) -> Option<Command> {
        let cleanup = self.cleanup.as_mut()?;
        match cleanup.phase {
            CleanupPhase::Stopping if !cleanup.stop_sent => {
                cleanup.stop_sent = true;
                Some(Command::StopAndDrain)
            }
            CleanupPhase::Disabling if !cleanup.disable_sent => {
                let disable = cleanup.disable.clone()?;
                cleanup.disable_sent = true;
                Some(Command::ConfigureStm32(disable))
            }
            _ => None,
        }
    }

    /// The submitter could not enqueue the command; allow a later attempt.
    fn cleanup_unsent(&mut self) {
        if let Some(cleanup) = &mut self.cleanup {
            match cleanup.phase {
                CleanupPhase::Stopping => cleanup.stop_sent = false,
                CleanupPhase::Disabling => cleanup.disable_sent = false,
                _ => {}
            }
        }
    }

    fn fail_cleanup(&mut self, error: Error) {
        let Some(cleanup) = &mut self.cleanup else {
            return;
        };
        cleanup.fail(error.clone());
        self.record_cleanup(error);
    }

    fn expire_cleanup(&mut self, now: Instant) {
        let expired = self.cleanup.as_ref().is_some_and(|cleanup| {
            matches!(
                cleanup.phase,
                CleanupPhase::Stopping | CleanupPhase::Disabling
            ) && now >= cleanup.deadline
        });
        if expired {
            self.fail_cleanup(Error::Timeout);
        }
    }

    fn cleanup_finished(&self) -> bool {
        matches!(
            self.cleanup_phase(),
            Some(CleanupPhase::Done | CleanupPhase::Failed)
        )
    }

    fn cleanup_stop_confirmed(&mut self) {
        let config = self.config.clone();
        let Some(cleanup) = &mut self.cleanup else {
            return;
        };
        let Some(mut disable) = config else {
            cleanup.fail(Error::InvalidState);
            return;
        };
        disable.loff = 0;
        cleanup.disable = Some(disable);
        cleanup.phase = CleanupPhase::Disabling;
    }

    fn cleanup_disable_confirmed(&mut self, config: Configuration) {
        let matched = self
            .cleanup
            .as_ref()
            .and_then(|cleanup| cleanup.disable.as_ref())
            .is_some_and(|disable| disable == &config);
        if !matched || config.loff != 0 {
            self.fail_cleanup(Error::InvalidConfig(
                "STM32 LOFF disable ACK does not match request".into(),
            ));
            return;
        }
        if let Some(cleanup) = &mut self.cleanup {
            cleanup.phase = CleanupPhase::Done;
        }
        self.config = Some(config);
        self.loff_may_be_on = false;
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
        // Post-fault LOFF cleanup may consume only the cleanup generation's ACKs;
        // late ACKs from failed normal controls are ignored and never change state.
        if self.error.is_some() {
            match event {
                Event::Disconnected => {
                    self.disconnected = true;
                    if self.cleanup_phase().is_some_and(|phase| {
                        matches!(phase, CleanupPhase::Stopping | CleanupPhase::Disabling)
                    }) {
                        self.fail_cleanup(Error::Disconnected);
                    }
                }
                Event::Stm32Stopped(result) => {
                    if self.stale_stop_acks > 0 {
                        // Consume pre-fault responses regardless of the current
                        // cleanup phase; the driver delivers controls in order.
                        self.stale_stop_acks -= 1;
                    } else if self.cleanup.as_ref().is_some_and(|cleanup| {
                        cleanup.phase == CleanupPhase::Stopping && cleanup.stop_sent
                    }) {
                        match result {
                            Ok(()) => self.cleanup_stop_confirmed(),
                            Err(error) => self.fail_cleanup(Error::Transport(error)),
                        }
                    } else if self.closing
                        && let Err(error) = result
                    {
                        // Cleanup diagnostics survive an earlier fault without
                        // changing state or masking the primary error.
                        self.record_cleanup(Error::Transport(error));
                    }
                }
                Event::Stm32Configured(result) => {
                    if self.stale_configure_acks > 0 {
                        // Consume pre-fault responses regardless of the current
                        // cleanup phase; the driver delivers controls in order.
                        self.stale_configure_acks -= 1;
                    } else if self.cleanup.as_ref().is_some_and(|cleanup| {
                        cleanup.phase == CleanupPhase::Disabling && cleanup.disable_sent
                    }) {
                        match result {
                            Ok(config) => self.cleanup_disable_confirmed(config),
                            Err(error) => self.fail_cleanup(Error::Transport(error)),
                        }
                    } else if self.closing
                        && let Err(error) = result
                    {
                        self.record_cleanup(Error::Transport(error));
                    }
                }
                Event::Error(message) => {
                    if self.cleanup_phase().is_some_and(|phase| {
                        matches!(phase, CleanupPhase::Stopping | CleanupPhase::Disabling)
                    }) {
                        self.fail_cleanup(Error::Transport(message.clone()));
                    }
                    if self.closing {
                        self.record_cleanup(Error::Transport(message));
                    }
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
                // The response is consumed even when it reports failure, so a
                // later fault must not treat it as an in-flight stale ACK.
                let pending = self.pending.take();
                let config = result.map_err(Error::InvalidConfig)?;
                config.validate().map_err(Error::InvalidConfig)?;
                match pending {
                    Some(Action::Configure(expected)) if expected == config => {}
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
                self.pending = None;
                result.map_err(Error::Transport)?;
                self.queue.clear();
                self.triggers.clear();
                self.parser = Parser::default();
                self.previous = None;
                self.index = 0;
                self.samples = 0;
                self.missing = 0;
                self.generation += 1;
                self.state = State::Streaming;
            }
            Event::Stm32Stopped(result) if matches!(self.pending, Some(Action::Stop)) => {
                self.pending = None;
                result.map_err(Error::Transport)?;
                self.state = State::Ready;
            }
            // A failed Disconnect stop leaves the latest SDK driver running until
            // Shutdown. Its best-effort cleanup report must not vanish during close.
            Event::Stm32Stopped(Err(error)) if self.pending.is_none() && self.closing => {
                self.record_cleanup(Error::Transport(error));
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
                let events = std::mem::take(&mut self.parser.events);
                let trigger_overflow = self.triggers.len() + events.len() > TRIGGER_CAPACITY;
                let remaining = TRIGGER_CAPACITY.saturating_sub(self.triggers.len());
                self.triggers.extend(events.into_iter().take(remaining));
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
                if trigger_overflow {
                    return Err(Error::Overflow);
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
                // Excitation that may still be on is stopped and disabled before
                // the link is released, even if the caller never calls close.
                if core.error.is_some() && !core.closing && control.is_empty() {
                    let now = Instant::now();
                    core.start_cleanup(now);
                    core.expire_cleanup(now);
                    if let Some(command) = core.cleanup_command() {
                        if control.try_send(command).is_err() {
                            core.cleanup_unsent();
                        }
                    } else if !core.loff_may_be_on || core.cleanup_finished() {
                        let _ = control.try_send(Command::Disconnect);
                    }
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
                // LOFF cleanup must own the transport; disconnect is safe only
                // when excitation cannot be on.
                let disconnect = !core.loff_may_be_on;
                core.fault(Error::Timeout);
                self.shared.changed.notify_all();
                if disconnect {
                    let _ = self.send(Command::Disconnect);
                }
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
            // The command never reached the driver, so no ACK for it can arrive.
            let mut core = self.shared.core.lock().unwrap();
            core.pending = None;
            core.control_deadline = None;
            core.fault(error.clone());
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
    /// Drain available hardware events independently of sample reads. Call regularly.
    /// Retain the received prefix on a fault, then report the fault on the next call.
    pub fn read_triggers(&self) -> Result<(u64, Vec<omnibci_sdk::protocol::EegTrigger>)> {
        let mut core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
        if core.triggers.is_empty()
            && let Some(error) = &core.error
        {
            return Err(error.clone());
        }
        let generation = core.generation;
        Ok((generation, core.triggers.drain(..).collect()))
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
    /// Drive the post-fault LOFF cleanup to completion within `deadline`. The
    /// worker usually starts it; close may submit the next step sooner and also
    /// bounds a faulted session whose cleanup is still waiting for an ACK.
    fn drive_fault_cleanup(&self, deadline: Instant) {
        {
            let Ok(mut core) = self.shared.core.lock() else {
                return;
            };
            core.start_cleanup(Instant::now());
        }
        loop {
            let command = {
                let Ok(mut core) = self.shared.core.lock() else {
                    return;
                };
                let now = Instant::now();
                core.expire_cleanup(now);
                core.cleanup_command()
            };
            if let Some(command) = command {
                if let Err(error) = self.send_before(command, deadline) {
                    if let Ok(mut core) = self.shared.core.lock() {
                        core.fail_cleanup(error);
                    }
                    return;
                }
                continue;
            }
            let finished = {
                let Ok(core) = self.shared.core.lock() else {
                    return;
                };
                core.cleanup_finished() || core.disconnected
            };
            if finished {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if let Ok(mut core) = self.shared.core.lock() {
                    core.fail_cleanup(Error::Timeout);
                }
                return;
            }
            let Ok(core) = self.shared.core.lock() else {
                return;
            };
            let _ = self
                .shared
                .changed
                .wait_timeout(core, remaining.min(Duration::from_millis(10)));
        }
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
        // The session may have faulted before close or while stopping above.
        // When excitation may still be on, attempt a bounded confirmed stop and
        // LOFF disable despite the primary fault, before Disconnect.
        let (faulted, loff_may_be_on, disconnected) = {
            let core = self.shared.core.lock().map_err(|_| Error::WorkerPanicked)?;
            (core.error.is_some(), core.loff_may_be_on, core.disconnected)
        };
        if faulted && loff_may_be_on && !disconnected {
            self.drive_fault_cleanup(deadline);
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
        let mut state = Core {
            config: Some(Configuration::default()),
            state: State::Ready,
            ..Default::default()
        };
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
        let mut state = Core {
            state: State::Ready,
            ..Default::default()
        };
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
        let mut state = Core {
            state: State::Streaming,
            ..Default::default()
        };
        state.event(Event::Disconnected).unwrap();
        assert_eq!(state.state, State::Faulted);
        assert_eq!(state.error, Some(Error::Disconnected));
    }
    #[test]
    fn stale_stop_ack_cannot_confirm_cleanup_stop() {
        let mut state = Core {
            state: State::Streaming,
            config: Some(Configuration {
                loff: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        state.loff_may_be_on = true;
        state.begin(Action::Stop).unwrap();
        state.fault(Error::Timeout);
        state.start_cleanup(Instant::now());
        assert!(matches!(
            state.cleanup_command(),
            Some(Command::StopAndDrain)
        ));
        // The pre-fault stop's late ACK cannot confirm the cleanup stop.
        state.event(Event::Stm32Stopped(Ok(()))).unwrap();
        assert_eq!(state.cleanup_phase(), Some(CleanupPhase::Stopping));
        state.event(Event::Stm32Stopped(Ok(()))).unwrap();
        assert_eq!(state.cleanup_phase(), Some(CleanupPhase::Disabling));
        assert_eq!(
            state
                .cleanup
                .as_ref()
                .unwrap()
                .disable
                .as_ref()
                .unwrap()
                .loff,
            0
        );
        assert_eq!(state.error, Some(Error::Timeout));
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
    fn wait_until(deadline: Instant, mut ready: impl FnMut() -> bool) {
        while Instant::now() < deadline {
            if ready() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("condition not reached before deadline");
    }
    fn loff_on_config() -> Configuration {
        Configuration {
            loff: 1,
            ..Default::default()
        }
    }
    /// Overflow while streaming with LOFF on must stop acquisition promptly,
    /// confirm LOFF off, and only then release the transport. The original
    /// overflow is still reported and no unconfirmed-excitation warning remains.
    #[test]
    fn prior_overflow_with_loff_on_confirms_stop_and_disable_before_disconnect() {
        let (log_tx, log_rx) = std::sync::mpsc::channel();
        let (mut device, events) = simulated(move |command, events| {
            log_tx.send(command.clone()).unwrap();
            acknowledge(command, events);
        });
        device
            .configure(loff_on_config(), Duration::from_secs(1))
            .unwrap();
        device.start(Duration::from_secs(1)).unwrap();
        for sequence in 0..=SAMPLE_CAPACITY {
            events.send(Event::Bytes(packet(sequence as u32))).unwrap();
        }
        wait_until(Instant::now() + Duration::from_secs(1), || {
            device.shared.core.lock().unwrap().error == Some(Error::Overflow)
        });
        // The worker stops acquisition without waiting for the caller to close.
        let mut log = Vec::new();
        loop {
            let command = log_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("worker must stop acquisition after the fault");
            log.push(command.clone());
            if matches!(command, Command::StopAndDrain) {
                break;
            }
        }
        wait_until(Instant::now() + Duration::from_secs(1), || {
            device.shared.core.lock().unwrap().cleanup_phase() == Some(CleanupPhase::Done)
        });
        let error = device
            .close(Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Overflow"), "{error}");
        assert!(!error.contains("could not be confirmed"), "{error}");
        while let Ok(command) = log_rx.try_recv() {
            log.push(command);
        }
        let stop = log
            .iter()
            .position(|command| matches!(command, Command::StopAndDrain))
            .expect("cleanup stop");
        let disable = log
            .iter()
            .position(
                |command| matches!(command, Command::ConfigureStm32(config) if config.loff == 0),
            )
            .expect("cleanup LOFF disable");
        assert!(stop < disable, "disable must follow the stop: {log:?}");
        let core = device.shared.core.lock().unwrap();
        assert_eq!(core.error, Some(Error::Overflow));
        assert!(!core.loff_may_be_on);
        assert_eq!(core.config.as_ref().unwrap().loff, 0);
    }
    /// A lost LOFF-disable ACK must keep the unconfirmed-excitation diagnostic
    /// and the primary fault, and shutdown must stay bounded.
    #[test]
    fn lost_disable_ack_retains_original_fault_and_unconfirmed_loff() {
        let (log_tx, log_rx) = std::sync::mpsc::channel();
        let (mut device, events) = simulated(move |command, events| {
            log_tx.send(command.clone()).unwrap();
            match &command {
                // Lose the cleanup acknowledgement; the request was delivered.
                Command::ConfigureStm32(config) if config.loff == 0 => {}
                _ => acknowledge(command, events),
            }
        });
        device
            .configure(loff_on_config(), Duration::from_secs(1))
            .unwrap();
        device.start(Duration::from_secs(1)).unwrap();
        for sequence in 0..=SAMPLE_CAPACITY {
            events.send(Event::Bytes(packet(sequence as u32))).unwrap();
        }
        wait_until(Instant::now() + Duration::from_secs(1), || {
            device.shared.core.lock().unwrap().error == Some(Error::Overflow)
        });
        let started = Instant::now();
        let error = device
            .close(Duration::from_millis(250))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < Duration::from_secs(1), "{error}");
        assert!(error.contains("Overflow"), "{error}");
        assert!(error.contains("LOFF"), "{error}");
        assert!(error.contains("could not be confirmed"), "{error}");
        let mut attempted = false;
        while let Ok(command) = log_rx.try_recv() {
            if matches!(command, Command::ConfigureStm32(config) if config.loff == 0) {
                attempted = true;
            }
        }
        assert!(attempted, "close must attempt the LOFF disable");
        let core = device.shared.core.lock().unwrap();
        assert_eq!(core.error, Some(Error::Overflow));
        assert!(core.loff_may_be_on);
    }
    /// A late ACK from a normal control that timed out before the fault must not
    /// confirm the cleanup disable; without a fresh ACK LOFF stays unconfirmed.
    #[test]
    fn late_normal_configure_ack_cannot_confirm_cleanup_disable() {
        let mut withheld = false;
        let mut stale_ack = None;
        let (stale_tx, stale_rx) = std::sync::mpsc::channel();
        let (mut device, _) = simulated(move |command, events| {
            match &command {
                Command::ConfigureStm32(config) if config.loff == 0 => {
                    // The first disable is the normal request whose ACK is late;
                    // replay it when the cleanup disable arrives, then lose the
                    // cleanup request's own ACK.
                    if !withheld {
                        withheld = true;
                        stale_ack = Some(config.clone());
                    } else if let Some(config) = stale_ack.take() {
                        events.send(Event::Stm32Configured(Ok(config))).unwrap();
                        stale_tx.send(()).unwrap();
                    }
                }
                _ => acknowledge(command, events),
            }
        });
        device
            .configure(loff_on_config(), Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            device.configure(
                Configuration {
                    loff: 0,
                    ..Default::default()
                },
                Duration::from_millis(5)
            ),
            Err(Error::Timeout)
        );
        stale_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("driver must replay the stale ACK");
        wait_until(Instant::now() + Duration::from_secs(1), || {
            device.shared.core.lock().unwrap().stale_configure_acks == 0
        });
        {
            let core = device.shared.core.lock().unwrap();
            assert_eq!(core.cleanup_phase(), Some(CleanupPhase::Disabling));
            assert!(core.loff_may_be_on, "stale ACK must not disable LOFF");
        }
        let error = device
            .close(Duration::from_millis(100))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Timeout"), "{error}");
        assert!(error.contains("could not be confirmed"), "{error}");
        assert!(device.shared.core.lock().unwrap().loff_may_be_on);
    }
    /// Cleanup that never gets a stop ACK must still release the link within the
    /// caller's close deadline while retaining the diagnostic.
    #[test]
    fn bounded_close_when_cleanup_is_unacknowledged() {
        let (mut device, events) = simulated(|command, events| match command {
            Command::StopAndDrain => {}
            Command::ConfigureStm32(config) if config.loff == 0 => {}
            _ => acknowledge(command, events),
        });
        device
            .configure(loff_on_config(), Duration::from_secs(1))
            .unwrap();
        device.start(Duration::from_secs(1)).unwrap();
        for sequence in 0..=SAMPLE_CAPACITY {
            events.send(Event::Bytes(packet(sequence as u32))).unwrap();
        }
        wait_until(Instant::now() + Duration::from_secs(1), || {
            device.shared.core.lock().unwrap().error == Some(Error::Overflow)
        });
        let started = Instant::now();
        let error = device
            .close(Duration::from_millis(50))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < Duration::from_secs(1), "{error}");
        assert!(error.contains("Overflow"), "{error}");
        assert!(error.contains("could not be confirmed"), "{error}");
        let core = device.shared.core.lock().unwrap();
        assert_eq!(core.error, Some(Error::Overflow));
        assert!(core.loff_may_be_on);
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
        // Cleanup is attempted after the fault; lose its ACKs and bound close.
        let error = device.close(Duration::from_millis(100)).unwrap_err();
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
#[cfg(test)]
mod trigger_queue_tests {
    use super::*;
    fn event_packet(code: u8, timestamp: u64) -> Vec<u8> {
        let mut p = vec![0u8; 48];
        p[..4].copy_from_slice(&[0xa5, 0x5a, 1, 2]);
        p[8..16].copy_from_slice(&timestamp.to_le_bytes());
        p[16] = code;
        p[17] = 1;
        p[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        p[22..26].copy_from_slice(&1000u32.to_le_bytes());
        let crc = omnibci_sdk::protocol::crc16_ccitt(&p[..46]);
        p[46..].copy_from_slice(&crc.to_le_bytes());
        p
    }
    fn core() -> Core {
        Core {
            state: State::Streaming,
            config: Some(Configuration::default()),
            generation: 3,
            ..Default::default()
        }
    }
    #[test]
    fn fragmented_event_only_packets_preserve_repeats_zero_and_exact_time() {
        let mut c = core();
        let timestamp = (1u64 << 53) + 123;
        let packets: Vec<u8> = [0, 7, 7, 255]
            .into_iter()
            .flat_map(|v| event_packet(v, timestamp))
            .collect();
        for chunk in packets.chunks(11) {
            c.event(Event::Bytes(chunk.to_vec())).unwrap();
        }
        assert!(c.queue.is_empty());
        assert_eq!(
            c.triggers.iter().map(|e| e.code).collect::<Vec<_>>(),
            [0, 7, 7, 255]
        );
        for e in &c.triggers {
            assert_eq!(e.sequence, u32::MAX);
            assert_eq!(e.timing.as_ref().unwrap().trigger_timestamp_us, timestamp);
            assert_eq!(e.timing.as_ref().unwrap().anchor_timestamp_us, 1000);
        }
    }
    #[test]
    fn stop_preserves_tail_and_start_clears_old_generation() {
        let mut c = core();
        c.begin(Action::Stop).unwrap();
        c.event(Event::Bytes(event_packet(7, 123))).unwrap();
        c.event(Event::Stm32Stopped(Ok(()))).unwrap();
        assert_eq!(c.triggers.len(), 1);
        c.begin(Action::Start).unwrap();
        assert_eq!(c.triggers.len(), 1);
        c.event(Event::Stm32Started(Ok(()))).unwrap();
        assert!(c.triggers.is_empty());
        assert_eq!(c.generation, 4);
    }
    #[test]
    fn overflow_reports_error_and_retains_prefix() {
        let mut c = core();
        let packets = event_packet(1, 123).repeat(TRIGGER_CAPACITY + 1);
        assert_eq!(c.event(Event::Bytes(packets)).unwrap_err(), Error::Overflow);
        assert_eq!(c.triggers.len(), TRIGGER_CAPACITY);
        assert!(c.triggers.iter().all(|e| e.code == 1));
    }
}

#[cfg(test)]
mod cleanup_ack_order_tests {
    use super::*;

    #[test]
    fn stale_acks_before_or_during_cleanup_do_not_consume_fresh_confirmation() {
        for stop in [false, true] {
            for early in [false, true] {
                let config = Configuration {
                    loff: 1,
                    bias: 0,
                    ..Default::default()
                };
                let mut off = config.clone();
                off.loff = 0;
                let mut core = Core {
                    state: State::Ready,
                    config: Some(config),
                    loff_may_be_on: true,
                    pending: Some(if stop {
                        Action::Stop
                    } else {
                        Action::Configure(off.clone())
                    }),
                    ..Default::default()
                };
                core.fault(Error::Timeout);
                if !early {
                    core.start_cleanup(Instant::now());
                    assert!(matches!(
                        core.cleanup_command(),
                        Some(Command::StopAndDrain)
                    ));
                }
                // The serialized driver can deliver the old configuration ACK
                // before it processes the cleanup STOP, or before cleanup starts.
                core.event(if stop {
                    Event::Stm32Stopped(Ok(()))
                } else {
                    Event::Stm32Configured(Ok(off.clone()))
                })
                .unwrap();
                assert!(core.loff_may_be_on);
                assert_eq!(core.stale_stop_acks, 0);
                assert_eq!(core.stale_configure_acks, 0);
                if early {
                    core.start_cleanup(Instant::now());
                    assert!(matches!(
                        core.cleanup_command(),
                        Some(Command::StopAndDrain)
                    ));
                }
                assert!(core.cleanup_phase() == Some(CleanupPhase::Stopping));
                core.event(Event::Stm32Stopped(Ok(()))).unwrap();
                assert!(matches!(
                    core.cleanup_command(),
                    Some(Command::ConfigureStm32(_))
                ));
                core.event(Event::Stm32Configured(Ok(off))).unwrap();
                assert!(!core.loff_may_be_on);
                assert_eq!(core.state, State::Faulted);
                assert_eq!(core.error, Some(Error::Timeout));
            }
        }
    }
}
