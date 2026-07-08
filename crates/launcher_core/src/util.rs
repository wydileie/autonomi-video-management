use std::{env, net::TcpListener as StdTcpListener, time::Duration};

use anyhow::Context;
use tracing::warn;

pub(crate) fn env_port_or_available(name: &str, default: u16) -> anyhow::Result<u16> {
    if let Ok(value) = env::var(name) {
        return value
            .parse::<u16>()
            .with_context(|| format!("{name} must be a valid TCP port"));
    }
    if port_available(default) {
        return Ok(default);
    }
    let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

pub(crate) fn port_available(port: u16) -> bool {
    StdTcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub(crate) fn env_duration(name: &str, default: Duration) -> Duration {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(default)
}

pub(crate) fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(err) = std::process::Command::new(opener).arg(url).spawn() {
        warn!("Could not open browser with {}: {}", opener, err);
    }
}
