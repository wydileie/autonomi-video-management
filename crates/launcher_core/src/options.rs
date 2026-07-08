use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkMode {
    Configured,
    LocalDevnet,
}

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub mode: NetworkMode,
    pub app_name: String,
    pub data_dir: Option<PathBuf>,
    pub binary_dir: Option<PathBuf>,
    pub frontend_dir: Option<PathBuf>,
    pub require_setup: bool,
    pub open_browser: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Configured,
            app_name: "Autonomi Video Management".to_string(),
            data_dir: None,
            binary_dir: None,
            frontend_dir: None,
            require_setup: false,
            open_browser: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FirstRunSetup {
    pub admin_username: String,
    pub admin_password: String,
    pub wallet_key: Option<String>,
    pub wallet_key_file: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupStatus {
    pub configured: bool,
    pub data_dir: String,
}
