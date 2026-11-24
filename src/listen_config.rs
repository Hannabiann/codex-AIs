//! Listener port from the native Web registry, with an explicit environment override.
use crate::db::registry::Registry;
use std::sync::atomic::{AtomicU16, Ordering};

const DEFAULT_PORT: u16 = 8070;
static BOUND_PORT: AtomicU16 = AtomicU16::new(0);

pub(crate) struct Selection {
    pub port: u16,
    pub source: &'static str,
}

pub(crate) fn select(registry: Option<&Registry>, override_value: Option<&str>) -> Result<Selection, String> {
    let (value, source, allow_zero) = if let Some(value) = override_value {
        (value, "KODEKS_PORT", true)
    } else if let Some(entry) = registry
        .map(|r| r.query(r.root, "Port"))
        .transpose()?
        .flatten()
    {
        (entry.value.as_str(), "config.db4: SOFTWARE\\Kodeks\\Web\\Port", false)
    } else {
        return Ok(Selection { port: DEFAULT_PORT, source: "default" });
    };
    let port = value.trim().parse::<u16>()
        .ok().filter(|&p| allow_zero || p != 0)
        .ok_or_else(|| format!("Invalid HTTP port in {source}: {value:?}; expected {}..65535", if allow_zero { 0 } else { 1 }))?;
    Ok(Selection { port, source })
}

pub(crate) fn configured(registry: Option<&Registry>) -> Result<Selection, String> {
    let value = std::env::var("KODEKS_PORT").map(Some).or_else(|e| match e {
        std::env::VarError::NotPresent => Ok(None),
        _ => Err("KODEKS_PORT is not valid Unicode".to_string()),
    })?;
    select(registry, value.as_deref())
}

// Set after bind and before starting KS workers. Includes an ephemeral port
// when KODEKS_PORT=0 is explicitly requested; no process environment mutation.
pub(crate) fn bound(port: u16) {
    BOUND_PORT.store(port, Ordering::Release);
}

pub(crate) fn effective_port(registry: Option<&Registry>) -> Result<u16, String> {
    let port = BOUND_PORT.load(Ordering::Acquire);
    if port != 0 {
        Ok(port)
    } else {
        Ok(configured(registry)?.port)
    }
}
