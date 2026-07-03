#![allow(clippy::unwrap_used)]
use std::{
    env,
    fs::{self},
};

use tokio::process::Command;

use super::*;

#[test]
fn setup_rejects_default_credentials() {
    let setup = FirstRunSetup {
        admin_username: "admin".to_string(),
        admin_password: "admin".to_string(),
        wallet_key: Some("0xabc".to_string()),
        wallet_key_file: None,
    };
    assert!(validate_setup(&setup).is_err());
}

#[test]
fn setup_accepts_strong_credentials_and_wallet_file() {
    let wallet_path =
        env::temp_dir().join(format!("autvid-wallet-key-test-{}", std::process::id()));
    fs::write(&wallet_path, "wallet-key").unwrap();
    let setup = FirstRunSetup {
        admin_username: "video-owner".to_string(),
        admin_password: "correct horse battery staple".to_string(),
        wallet_key: None,
        wallet_key_file: Some(wallet_path.to_string_lossy().to_string()),
    };
    assert!(validate_setup(&setup).is_ok());
    let _ = fs::remove_file(wallet_path);
}

#[test]
fn setup_rejects_missing_wallet_file() {
    let setup = FirstRunSetup {
        admin_username: "video-owner".to_string(),
        admin_password: "correct horse battery staple".to_string(),
        wallet_key: None,
        wallet_key_file: Some("/tmp/autvid-missing-wallet-key".to_string()),
    };
    assert!(validate_setup(&setup).is_err());
}

#[test]
fn env_port_uses_env_value() {
    env::set_var("AUTVID_TEST_PORT", "45678");
    assert_eq!(env_port_or_available("AUTVID_TEST_PORT", 1).unwrap(), 45678);
    env::remove_var("AUTVID_TEST_PORT");
}

#[test]
fn desktop_admin_auth_env_is_applied_to_admin_command() {
    let mut command = Command::new("rust_admin");
    apply_desktop_admin_auth_env(&mut command);

    for (expected_key, expected_value) in desktop_admin_auth_env() {
        let value = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new(expected_key))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(value.as_deref(), Some(*expected_value));
    }
}

#[test]
fn stream_proxy_preserves_stream_prefix_for_sidecar_routes() {
    assert_eq!(
        stream_proxy_path("manifest/address/720p/playlist.m3u8"),
        "/stream/manifest/address/720p/playlist.m3u8"
    );
}

#[cfg(unix)]
#[test]
fn process_name_matches_target_triple_sidecar() {
    assert!(process_name_is_expected("rust_admin", "rust_admin"));
    assert!(process_name_is_expected(
        "rust_admin-aarch64-apple-darwin",
        "rust_admin"
    ));
    assert!(!process_name_is_expected("rust_admin_backup", "rust_admin"));
}
