//! Find receivers via Bonjour (§2.1).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use opendisplay_proto::SERVICE_TYPE;
use tracing::info;

#[derive(Debug, Clone)]
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

/// Browse for `timeout`, calling `on_found` for each resolved receiver
/// until it returns `true`.
fn browse(timeout: Duration, mut on_found: impl FnMut(Found) -> bool) -> Result<()> {
    let daemon = ServiceDaemon::new().context("starting mDNS browser")?;
    let ty = format!("{SERVICE_TYPE}.local.");
    let rx = daemon.browse(&ty).context("browsing")?;
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
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
                if found.pv >= 1 && on_found(found) {
                    break;
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(&ty);
    let _ = daemon.shutdown();
    Ok(())
}

/// Browse until a receiver (optionally one matching `to`, see
/// [`Found::matches`]) resolves, or `timeout` passes.
pub fn find(to: Option<&str>, timeout: Duration) -> Result<Found> {
    let mut hit = None;
    browse(timeout, |f| {
        if to.is_some_and(|t| !f.matches(t)) {
            info!("skipping {}", f.instance);
            return false;
        }
        hit = Some(f);
        true
    })?;
    match hit {
        Some(f) => Ok(f),
        None => bail!("no receiver found within {timeout:?}"),
    }
}

/// Every receiver that resolves within `timeout`, one entry per instance.
pub fn list(timeout: Duration) -> Result<Vec<Found>> {
    let mut all: Vec<Found> = Vec::new();
    browse(timeout, |f| {
        if !all.iter().any(|a| a.instance == f.instance) {
            all.push(f);
        }
        false
    })?;
    all.sort_by(|a, b| a.instance.cmp(&b.instance));
    Ok(all)
}
