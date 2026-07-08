//! Core launcher: first-run setup, stack process orchestration, and the
//! local HTTP proxy used by the standalone launcher and desktop app.
mod options;
mod process;
mod proxy;
mod setup;
mod stack;
mod tools;
mod util;

pub use options::*;
pub(crate) use process::*;
pub(crate) use proxy::*;
pub use setup::*;
pub use stack::*;
pub(crate) use tools::*;
pub(crate) use util::*;

const CONFIG_FILE: &str = "desktop-config.json";
const PASSWORD_FILE: &str = "admin-password";
const AUTH_SECRET_FILE: &str = "admin-auth-secret";
const WALLET_KEY_FILE: &str = "autonomi-wallet-key";
const MANAGED_CHILD_NAMES: &[&str] = &["antd", "local-devnet", "rust_admin", "rust_stream"];

#[cfg(test)]
mod tests;
