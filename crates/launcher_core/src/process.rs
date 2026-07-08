use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

use anyhow::{anyhow, Context};
use fs2::FileExt;
use tokio::process::Child;
use tracing::warn;

use super::*;

pub(crate) fn acquire_instance_lock(run_dir: &Path) -> anyhow::Result<File> {
    let lock_path = run_dir.join("launcher.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open lock file {}", lock_path.to_string_lossy()))?;
    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(lock_file),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Err(anyhow!(
            "another Autonomi Video Management instance is already using {}",
            run_dir.parent().unwrap_or(run_dir).to_string_lossy()
        )),
        Err(err) => Err(err).with_context(|| {
            format!(
                "could not lock launcher instance file {}",
                lock_path.to_string_lossy()
            )
        }),
    }
}

pub(crate) fn cleanup_stale_children(run_dir: &Path) -> anyhow::Result<()> {
    for name in MANAGED_CHILD_NAMES {
        let pid_file = run_dir.join(format!("{name}.pid"));
        let raw = match fs::read_to_string(&pid_file) {
            Ok(value) => value,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warn!(
                    "Could not read stale {} pid file {}: {}",
                    name,
                    pid_file.to_string_lossy(),
                    err
                );
                continue;
            }
        };
        let Ok(pid) = raw.trim().parse::<u32>() else {
            warn!(
                "Removing invalid stale {} pid file {}",
                name,
                pid_file.to_string_lossy()
            );
            let _ = fs::remove_file(&pid_file);
            continue;
        };
        if stale_child_running(pid, name) {
            warn!("Stopping stale {} sidecar with pid {}", name, pid);
            if let Err(err) = terminate_pid(pid) {
                warn!("Could not stop stale {} sidecar {}: {}", name, pid, err);
            }
        }
        let _ = fs::remove_file(&pid_file);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    terminate_pid(pid)
}

#[cfg(unix)]
pub(crate) fn terminate_pid(pid: u32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    child.start_kill()
}

#[cfg(not(unix))]
pub(crate) fn terminate_pid(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stale pid cleanup is not implemented on this platform",
    ))
}

#[cfg(unix)]
pub(crate) fn stale_child_running(pid: u32, expected_name: &str) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result != 0 {
        return false;
    }
    process_name_matches(pid, expected_name).unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn stale_child_running(_pid: u32, _expected_name: &str) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub(crate) fn process_name_matches(pid: u32, expected_name: &str) -> Option<bool> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(process_name_is_expected(comm.trim(), expected_name))
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn process_name_matches(pid: u32, expected_name: &str) -> Option<bool> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(false);
    }
    let command = String::from_utf8_lossy(&output.stdout);
    Some(
        Path::new(command.trim())
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|name| process_name_is_expected(name, expected_name)),
    )
}

#[cfg(unix)]
pub(crate) fn process_name_is_expected(candidate: &str, expected_name: &str) -> bool {
    candidate == expected_name
        || candidate
            .strip_prefix(expected_name)
            .is_some_and(|suffix| suffix.starts_with('-'))
}
