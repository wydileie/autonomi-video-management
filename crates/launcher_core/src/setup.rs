use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DesktopConfig {
    pub(crate) schema_version: u8,
    pub(crate) admin_username: String,
    pub(crate) admin_password_file: PathBuf,
    pub(crate) admin_auth_secret_file: PathBuf,
    pub(crate) wallet_key_file: Option<PathBuf>,
}

pub fn resolve_data_dir(app_name: &str) -> anyhow::Result<PathBuf> {
    if let Ok(value) = env::var("AUTVID_DATA_DIR") {
        return Ok(PathBuf::from(value));
    }
    let home = env::var("HOME").context("HOME is required when AUTVID_DATA_DIR is unset")?;
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join(app_name))
    } else {
        let linux_name = app_name
            .to_ascii_lowercase()
            .replace(' ', "-")
            .replace("--", "-");
        Ok(env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".local").join("share"))
            .join(linux_name))
    }
}

pub fn setup_status(data_dir: &Path) -> SetupStatus {
    SetupStatus {
        configured: read_desktop_config(data_dir).is_ok(),
        data_dir: data_dir.to_string_lossy().to_string(),
    }
}

pub fn save_first_run_config(data_dir: &Path, setup: FirstRunSetup) -> anyhow::Result<SetupStatus> {
    validate_setup(&setup)?;
    fs::create_dir_all(data_dir)?;
    let secrets_dir = data_dir.join("secrets");
    fs::create_dir_all(&secrets_dir)?;
    set_private_dir_permissions(&secrets_dir)?;

    let password_file = secrets_dir.join(PASSWORD_FILE);
    write_secret_file(&password_file, setup.admin_password.as_bytes())?;
    let auth_secret_file = secrets_dir.join(AUTH_SECRET_FILE);
    write_secret_file(&auth_secret_file, random_hex_secret().as_bytes())?;

    let wallet_key_file = match (
        setup.wallet_key.map(|value| value.trim().to_string()),
        setup.wallet_key_file.map(|value| value.trim().to_string()),
    ) {
        (Some(key), _) if !key.is_empty() => {
            let path = secrets_dir.join(WALLET_KEY_FILE);
            write_secret_file(&path, key.as_bytes())?;
            Some(path)
        }
        (_, Some(path)) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => None,
    };

    let config = DesktopConfig {
        schema_version: 1,
        admin_username: setup.admin_username.trim().to_string(),
        admin_password_file: password_file,
        admin_auth_secret_file: auth_secret_file,
        wallet_key_file,
    };
    let encoded = serde_json::to_vec_pretty(&config)?;
    write_private_file(&data_dir.join(CONFIG_FILE), &encoded)?;
    Ok(setup_status(data_dir))
}

pub(crate) fn validate_setup(setup: &FirstRunSetup) -> anyhow::Result<()> {
    let username = setup.admin_username.trim();
    if username.is_empty() || is_unsafe_admin_auth_value(username) {
        return Err(anyhow!("choose a non-default admin username"));
    }
    let password = setup.admin_password.trim();
    if password.len() < 12 || is_unsafe_admin_auth_value(password) {
        return Err(anyhow!(
            "admin password must be at least 12 characters and non-default"
        ));
    }
    if setup
        .wallet_key
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && setup
            .wallet_key_file
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(anyhow!(
            "provide an Autonomi wallet key or wallet key file path"
        ));
    }
    if let Some(path) = setup
        .wallet_key_file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(anyhow!(
                "Autonomi wallet key file does not exist or is not a file"
            ));
        }
    }
    Ok(())
}

pub(crate) fn random_hex_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn read_desktop_config(data_dir: &Path) -> anyhow::Result<DesktopConfig> {
    let config_path = data_dir.join(CONFIG_FILE);
    let raw = fs::read(&config_path).with_context(|| {
        format!(
            "could not read desktop config at {}",
            config_path.to_string_lossy()
        )
    })?;
    let config: DesktopConfig = serde_json::from_slice(&raw)?;
    if config.schema_version != 1 {
        return Err(anyhow!("unsupported desktop config schema"));
    }
    if !config.admin_password_file.is_file() || !config.admin_auth_secret_file.is_file() {
        return Err(anyhow!("desktop setup secret files are missing"));
    }
    Ok(config)
}

pub(crate) fn write_secret_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut contents = bytes.to_vec();
    contents.push(b'\n');
    write_private_file(path, &contents)
}

pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    set_private_file_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_private_dir_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn set_private_dir_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn set_private_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn is_unsafe_admin_auth_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "admin"
            | "administrator"
            | "changeme"
            | "change-me"
            | "change_me"
            | "default"
            | "password"
            | "please-change-me"
            | "replace-me"
            | "secret"
            | "test"
            | "test-secret"
    ) || [
        "change-me",
        "change_me",
        "changeme",
        "change-this",
        "change_this",
        "changethis",
        "replace-me",
        "replace_me",
        "replace-this",
        "replace_this",
    ]
    .iter()
    .any(|placeholder| normalized.contains(placeholder))
}
