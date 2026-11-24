//! Thin ESB dongle discovery and board-selection adapter over omnibci-sdk.
use omnibci_sdk::{
    discovery::{
        Cancellation, Candidate, Link,
        dongle::{Dongle, Mode},
        usb_candidates,
    },
    session::{Error, Stm32Device},
    stm32::{Configuration, spawn_selected_dongle},
};
use std::time::{Duration, Instant};

pub(crate) fn parse_esb_address(address: &str) -> Result<(&str, Option<u64>), Error> {
    let (port, query) = match address.split_once('?') {
        Some((port, query)) => (port, Some(query)),
        None => (address, None),
    };
    if port.is_empty() {
        return Err(Error::InvalidConfig(
            "ESB endpoint must be esb://PORT or esb://PORT?board=<16-hex-id>".into(),
        ));
    }
    let Some(query) = query else {
        return Ok((port, None));
    };
    let Some(hex) = query.strip_prefix("board=") else {
        return Err(Error::InvalidConfig(
            "ESB endpoint query must be ?board=<16-hex-id>".into(),
        ));
    };
    if hex.len() != 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::InvalidConfig(
            "ESB board ID must be a 16-character hexadecimal string".into(),
        ));
    }
    let id = u64::from_str_radix(hex, 16)
        .map_err(|_| Error::InvalidConfig("invalid ESB board ID".into()))?;
    if id == 0 {
        return Err(Error::InvalidConfig("ESB board ID must be non-zero".into()));
    }
    Ok((port, Some(id)))
}

fn find_esb_candidate(port: &str) -> Result<Candidate, Error> {
    let candidates = usb_candidates().map_err(Error::Transport)?;
    candidates
        .into_iter()
        .find(|candidate| {
            candidate.link == Link::Esb
                && if cfg!(windows) {
                    candidate.key.eq_ignore_ascii_case(port)
                } else {
                    candidate.key == port
                }
        })
        .ok_or_else(|| Error::InvalidConfig(format!("ESB dongle not found on serial port {port}")))
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, Error> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::Timeout);
    }
    Ok(remaining)
}

pub(crate) fn discover_dongles(timeout: Duration) -> Result<serde_json::Value, Error> {
    let deadline = Instant::now() + timeout;
    let candidates: Vec<Candidate> = usb_candidates()
        .map_err(Error::Transport)?
        .into_iter()
        .filter(|candidate| candidate.link == Link::Esb)
        .collect();
    let cancel = Cancellation::default();
    let mut dongles = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let _ = remaining_timeout(deadline)?;
        let (mode, firmware_version) =
            Dongle::open_with_version(&candidate, &cancel).map_err(Error::Transport)?;
        let (mode_name, boards) = match mode {
            Mode::Search(dongle) => {
                let _ = remaining_timeout(deadline)?;
                let scanned = dongle.scan_boards(&cancel).map_err(Error::Transport)?;
                let boards = scanned
                    .into_iter()
                    .map(|board| {
                        serde_json::json!({
                            "id": format!("{:016X}", board.id),
                            "port": candidate.key,
                            "rssi_dbm": board.rssi_dbm,
                        })
                    })
                    .collect::<Vec<_>>();
                ("search", boards)
            }
            Mode::Bound => ("bound", Vec::new()),
            Mode::Legacy => ("legacy", Vec::new()),
        };
        dongles.push(serde_json::json!({
            "port": candidate.key,
            "mode": mode_name,
            "firmware_version": firmware_version,
            "boards": boards,
        }));
    }
    Ok(serde_json::Value::Array(dongles))
}

pub(crate) fn connect(address: &str, timeout: Duration) -> Result<Stm32Device, Error> {
    let (port, board_id) = parse_esb_address(address)?;
    let deadline = Instant::now() + timeout;
    let candidate = find_esb_candidate(port)?;
    let cancel = Cancellation::default();
    let (mode, _version) =
        Dongle::open_with_version(&candidate, &cancel).map_err(Error::Transport)?;
    match mode {
        Mode::Search(dongle) => {
            let Some(id) = board_id else {
                return Err(Error::InvalidConfig(
                    "ESB dongle is in search mode; specify a board with esb://PORT?board=<16-hex-id>"
                        .into(),
                ));
            };
            let _ = remaining_timeout(deadline)?;
            let identity = dongle.identify(id, &cancel).map_err(Error::Transport)?;
            let remaining = remaining_timeout(deadline)?;
            Stm32Device::connect_with_driver(
                remaining,
                move |commands, sink| {
                    spawn_selected_dongle(
                        dongle,
                        candidate,
                        identity,
                        Configuration::default(),
                        commands,
                        sink,
                        Cancellation::default(),
                    )
                },
                None,
            )
        }
        Mode::Bound | Mode::Legacy => {
            if board_id.is_some() {
                return Err(Error::InvalidConfig(
                    "ESB dongle is bound/legacy and does not support selecting a board ID; connect with esb://PORT"
                        .into(),
                ));
            }
            let remaining = remaining_timeout(deadline)?;
            Stm32Device::connect("serial", &candidate.key, remaining)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_esb_address_accepts_port_and_board_id() {
        assert_eq!(parse_esb_address("COM28").unwrap(), ("COM28", None));
        assert_eq!(
            parse_esb_address("/dev/ttyACM0").unwrap(),
            ("/dev/ttyACM0", None)
        );
        assert_eq!(
            parse_esb_address("COM28?board=A344B3C4FCB2E83F").unwrap(),
            ("COM28", Some(0xA344_B3C4_FCB2_E83F))
        );
        assert_eq!(
            parse_esb_address("/dev/ttyACM0?board=a344b3c4fcb2e83f").unwrap(),
            ("/dev/ttyACM0", Some(0xA344_B3C4_FCB2_E83F))
        );
    }

    #[test]
    fn parse_esb_address_rejects_malformed_inputs() {
        for bad in [
            "",
            "?board=A344B3C4FCB2E83F",
            "COM28?",
            "COM28?id=A344B3C4FCB2E83F",
            "COM28?board=",
            "COM28?board=A344",
            "COM28?board=ZZZZZZZZZZZZZZZZ",
            "COM28?board=0000000000000000",
            "COM28?board=A344B3C4FCB2E83F&extra=1",
        ] {
            assert!(
                parse_esb_address(bad).is_err(),
                "expected error for ESB address {bad:?}"
            );
        }
    }
}
