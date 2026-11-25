//! Listener port from the native Web registry, with an explicit environment override.
use crate::db::registry::Registry;
use std::sync::atomic::{AtomicU16, Ordering};

const DEFAULT_PORT: u16 = 8070;
static BOUND_PORT: AtomicU16 = AtomicU16::new(0);
static ADDRESS: std::sync::Mutex<Option<std::net::SocketAddr>> = std::sync::Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    All,
    Localhost,
}
impl Scope {
    pub fn text(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Localhost => "localhost",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "localhost" => Ok(Self::Localhost),
            _ => Err("Сетевые интерфейсы: выберите all или localhost".into()),
        }
    }
}
pub(crate) struct Host {
    pub value: String,
    pub source: &'static str,
}
pub(crate) fn select_host(scope: Scope, explicit: Option<&str>) -> Result<Host, String> {
    if let Some(value) = explicit {
        if value.trim().is_empty() {
            return Err("KODEKS_HOST must not be empty".into());
        }
        return Ok(Host {
            value: value.trim().into(),
            source: "KODEKS_HOST",
        });
    }
    Ok(Host {
        value: match scope {
            Scope::All => "0.0.0.0",
            Scope::Localhost => "127.0.0.1",
        }
        .into(),
        source: "настройки Rust",
    })
}
pub(crate) fn host(scope: Scope) -> Result<Host, String> {
    let explicit = std::env::var("KODEKS_HOST")
        .map(Some)
        .or_else(|e| match e {
            std::env::VarError::NotPresent => Ok(None),
            _ => Err("KODEKS_HOST is not valid Unicode".to_string()),
        })?;
    select_host(scope, explicit.as_deref())
}
pub(crate) fn data_root(
    explicit: Option<std::path::PathBuf>,
    exe: &std::path::Path,
    cwd: &std::path::Path,
    serve: bool,
) -> std::path::PathBuf {
    explicit.unwrap_or_else(|| {
        if serve && exe.parent().is_some_and(|p| p.join("config.db4").is_file()) {
            exe.parent().unwrap().to_path_buf()
        } else {
            cwd.to_path_buf()
        }
    })
}
pub(crate) fn address() -> Option<std::net::SocketAddr> {
    *ADDRESS.lock().unwrap()
}

pub(crate) struct Selection {
    pub port: u16,
    pub source: &'static str,
}

pub(crate) fn select(
    registry: Option<&Registry>,
    override_value: Option<&str>,
) -> Result<Selection, String> {
    let (value, source, allow_zero) = if let Some(value) = override_value {
        (value, "KODEKS_PORT", true)
    } else if let Some(entry) = registry
        .map(|r| r.query(r.root, "Port"))
        .transpose()?
        .flatten()
    {
        (
            entry.value.as_str(),
            "config.db4: SOFTWARE\\Kodeks\\Web\\Port",
            false,
        )
    } else {
        return Ok(Selection {
            port: DEFAULT_PORT,
            source: "default",
        });
    };
    let port = value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|&p| allow_zero || p != 0)
        .ok_or_else(|| {
            format!(
                "Invalid HTTP port in {source}: {value:?}; expected {}..65535",
                if allow_zero { 0 } else { 1 }
            )
        })?;
    Ok(Selection { port, source })
}

pub(crate) fn configured(registry: Option<&Registry>) -> Result<Selection, String> {
    let value = std::env::var("KODEKS_PORT")
        .map(Some)
        .or_else(|e| match e {
            std::env::VarError::NotPresent => Ok(None),
            _ => Err("KODEKS_PORT is not valid Unicode".to_string()),
        })?;
    select(registry, value.as_deref())
}

// Set after bind and before starting KS workers. Includes an ephemeral port
// when KODEKS_PORT=0 is explicitly requested; no process environment mutation.
pub(crate) fn bound(address: std::net::SocketAddr) {
    BOUND_PORT.store(address.port(), Ordering::Release);
    *ADDRESS.lock().unwrap() = Some(address);
}

pub(crate) fn effective_port(registry: Option<&Registry>) -> Result<u16, String> {
    let port = BOUND_PORT.load(Ordering::Acquire);
    if port != 0 {
        Ok(port)
    } else {
        Ok(configured(registry)?.port)
    }
}
