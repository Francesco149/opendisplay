//! Find receivers via Bonjour (§2.1).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use opendisplay_proto::SERVICE_TYPE;
use tracing::info;

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub instance: String,
    pub addr: SocketAddr,
    pub id: Option<String>,
    pub pv: u32,
}

impl Found {
    /// `--to` matching: the instance name contains `to`, or the receiver
    /// `id` equals it (case-insensitive).
    pub fn matches(&self, to: &str) -> bool {
        self.instance.contains(to)
            || self
                .id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(to))
    }
}

/// What to check when no receiver answers (PROTOCOL §2.1: Bonjour on the LAN).
pub const NO_RECEIVER_HINT: &str = "\
Is the OpenDisplay app open and in the foreground on the device, on the same
Wi-Fi as this computer (not a guest network)? On iPhone and iPad, Settings >
Privacy & Security > Local Network must allow OpenDisplay. Try
`od-sender list --timeout 10`, or connect directly with
`od-sender run --to <device-ip>:9000`.";

/// Discovery found nothing.
#[derive(Debug)]
pub struct NoReceiver {
    pub to: Option<String>,
    pub timeout: Duration,
}

impl std::fmt::Display for NoReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.to {
            Some(t) => write!(
                f,
                "no receiver matching {t:?} found within {:?}",
                self.timeout
            )?,
            None => write!(f, "no receiver found within {:?}", self.timeout)?,
        }
        write!(f, ".\n{NO_RECEIVER_HINT}")
    }
}

impl std::error::Error for NoReceiver {}

/// Several receivers answered and none was chosen.
#[derive(Debug)]
pub struct SeveralReceivers(pub Vec<Found>);

impl std::fmt::Display for SeveralReceivers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "several receivers found; pick one with --to <name>:")?;
        for r in &self.0 {
            writeln!(
                f,
                "  {}  ({}, id {})",
                r.instance,
                r.addr,
                r.id.as_deref().unwrap_or("-")
            )?;
        }
        write!(
            f,
            "(or set `to = \"<name>\"` in ~/.config/opendisplay/sender.toml)"
        )
    }
}

impl std::error::Error for SeveralReceivers {}

/// With no `--to`, keep listening this long after the first answer, so a
/// second device on the network is noticed instead of silently ignored.
const SETTLE_WITHOUT_TO: Duration = Duration::from_millis(1500);

/// Browse until `deadline` (`None`: forever), calling `on_found` for each
/// resolved receiver; it may move the deadline, or return `true` to stop.
fn browse(
    mut deadline: Option<Instant>,
    mut on_found: impl FnMut(Found, &mut Option<Instant>) -> bool,
) -> Result<()> {
    let daemon = ServiceDaemon::new().context("starting mDNS browser")?;
    let ty = format!("{SERVICE_TYPE}.local.");
    let rx = daemon.browse(&ty).context("browsing")?;
    loop {
        let wait = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                left
            }
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let instance = info
                    .get_fullname()
                    .trim_end_matches(&format!(".{ty}"))
                    .to_string();
                // Prefer IPv4: link-local IPv6 needs a scope id we do not have (§6.1 addrs note).
                let ip = info
                    .get_addresses()
                    .iter()
                    .map(|a| a.to_ip_addr())
                    .min_by_key(|a| if a.is_ipv4() { 0 } else { 1 });
                let Some(ip) = ip else { continue };
                let id = info.get_property_val_str("id").map(str::to_owned);
                let pv = info
                    .get_property_val_str("pv")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(1);
                let found = Found {
                    instance,
                    addr: SocketAddr::new(ip, info.get_port()),
                    id,
                    pv,
                };
                if found.pv >= 1 && on_found(found, &mut deadline) {
                    break;
                }
            }
            Ok(_) => continue,
            // Browsing without a deadline (`--wait`): a quiet hour is fine.
            Err(_) if deadline.is_none() && !rx.is_disconnected() => continue,
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(&ty);
    let _ = daemon.shutdown();
    Ok(())
}

/// The outcome of discovery, decided from what answered.
#[derive(Debug, PartialEq)]
pub enum Choice {
    One(Found),
    Several(Vec<Found>),
    None,
}

/// With `to`, the first match; without, the only receiver, or all of them.
pub fn choose(found: Vec<Found>, to: Option<&str>) -> Choice {
    let mut matching: Vec<Found> = match to {
        Some(t) => found.into_iter().filter(|f| f.matches(t)).take(1).collect(),
        None => found,
    };
    matching.dedup_by(|a, b| a.instance == b.instance);
    match matching.len() {
        0 => Choice::None,
        1 => Choice::One(matching.remove(0)),
        _ => Choice::Several(matching),
    }
}

/// Find the receiver to dial: the first matching `to`, or, without `to`,
/// the only one on the network (several is an error). `timeout: None`
/// browses until one appears (`run --wait`).
pub fn find(to: Option<&str>, timeout: Option<Duration>) -> Result<Found> {
    let mut seen: Vec<Found> = Vec::new();
    browse(timeout.map(|t| Instant::now() + t), |f, deadline| {
        if seen.iter().any(|s| s.instance == f.instance) {
            return false;
        }
        if let Some(t) = to {
            if !f.matches(t) {
                info!("skipping {}", f.instance);
                return false;
            }
            seen.push(f);
            return true;
        }
        if seen.is_empty() {
            // Give other receivers a moment to answer.
            *deadline = Some(Instant::now() + SETTLE_WITHOUT_TO);
        }
        seen.push(f);
        false
    })?;
    match choose(seen, to) {
        Choice::One(f) => Ok(f),
        Choice::Several(all) => Err(SeveralReceivers(all).into()),
        Choice::None => Err(NoReceiver {
            to: to.map(Into::into),
            timeout: timeout.unwrap_or_default(),
        }
        .into()),
    }
}

/// Every receiver that resolves within `timeout`, one entry per instance.
pub fn list(timeout: Duration) -> Result<Vec<Found>> {
    let mut all: Vec<Found> = Vec::new();
    browse(Some(Instant::now() + timeout), |f, _| {
        if !all.iter().any(|a| a.instance == f.instance) {
            all.push(f);
        }
        false
    })?;
    all.sort_by(|a, b| a.instance.cmp(&b.instance));
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, id: &str) -> Found {
        Found {
            instance: name.into(),
            addr: "192.168.1.20:9000".parse().unwrap(),
            id: Some(id.into()),
            pv: 3,
        }
    }

    #[test]
    fn without_to_one_receiver_is_used_and_several_are_refused() {
        assert_eq!(
            choose(vec![r("iPhone", "A")], None),
            Choice::One(r("iPhone", "A"))
        );
        assert!(matches!(
            choose(vec![r("iPhone", "A"), r("iMac", "B")], None),
            Choice::Several(v) if v.len() == 2
        ));
        assert_eq!(choose(vec![], None), Choice::None);
    }

    #[test]
    fn to_picks_by_name_or_id() {
        let all = vec![r("iPhone", "A1"), r("iMac", "B2")];
        assert_eq!(
            choose(all.clone(), Some("iMac")),
            Choice::One(r("iMac", "B2"))
        );
        assert_eq!(
            choose(all.clone(), Some("a1")),
            Choice::One(r("iPhone", "A1"))
        );
        assert_eq!(choose(all, Some("Pixel")), Choice::None);
    }

    #[test]
    fn errors_carry_the_hints() {
        let e = NoReceiver {
            to: None,
            timeout: Duration::from_secs(10),
        }
        .to_string();
        assert!(
            e.contains("Local Network") && e.contains("--to <device-ip>:9000"),
            "{e}"
        );
        let e = SeveralReceivers(vec![r("iPhone", "A"), r("iMac", "B")]).to_string();
        assert!(
            e.contains("pick one with --to") && e.contains("iMac"),
            "{e}"
        );
    }
}
