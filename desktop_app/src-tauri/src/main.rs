use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use launcher_core::{launch_stack, resolve_data_dir, save_first_run_config, setup_status};
use serde::{Deserialize, Serialize};
use tauri::{Manager, Runtime};
use tokio::sync::Mutex;

struct DesktopState {
    data_dir: PathBuf,
    closing: AtomicBool,
    running: Mutex<Option<launcher_core::RunningStack>>,
}

#[derive(Deserialize)]
struct DesktopSetupRequest {
    #[serde(rename = "adminUsername")]
    admin_username: String,
    #[serde(rename = "adminPassword")]
    admin_password: String,
    #[serde(rename = "walletKey")]
    wallet_key: Option<String>,
    #[serde(rename = "walletKeyFile")]
    wallet_key_file: Option<String>,
}

#[derive(Serialize)]
struct LaunchResponse {
    url: String,
}

#[tauri::command]
async fn desktop_setup_status(
    state: tauri::State<'_, Arc<DesktopState>>,
) -> Result<launcher_core::SetupStatus, String> {
    Ok(setup_status(&state.data_dir))
}

#[tauri::command]
async fn desktop_save_setup(
    state: tauri::State<'_, Arc<DesktopState>>,
    setup: DesktopSetupRequest,
) -> Result<launcher_core::SetupStatus, String> {
    save_first_run_config(
        &state.data_dir,
        launcher_core::FirstRunSetup {
            admin_username: setup.admin_username,
            admin_password: setup.admin_password,
            wallet_key: setup.wallet_key,
            wallet_key_file: setup.wallet_key_file,
        },
    )
    .map_err(|err| err.to_string())
}

#[tauri::command]
async fn desktop_start_stack<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, Arc<DesktopState>>,
) -> Result<LaunchResponse, String> {
    let mut running = state.running.lock().await;
    if let Some(stack) = running.as_ref() {
        return Ok(LaunchResponse {
            url: stack.url.clone(),
        });
    }

    let frontend_dir = resolve_frontend_dir(&app);
    let binary_dir = resolve_binary_dir(&app);
    let stack = launch_stack(launcher_core::LaunchOptions {
        mode: launcher_core::NetworkMode::Configured,
        app_name: "Autonomi Video Management".to_string(),
        data_dir: Some(state.data_dir.clone()),
        binary_dir,
        frontend_dir,
        require_setup: true,
        open_browser: false,
    })
    .await
    .map_err(|err| err.to_string())?;
    let url = stack.url.clone();
    *running = Some(stack);
    Ok(LaunchResponse { url })
}

fn resolve_frontend_dir<R: Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .resolve("frontend", tauri::path::BaseDirectory::Resource)
        .ok()
        .filter(|path| path.join("index.html").is_file())
        .or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../react_frontend/build")
                .canonicalize()
                .ok()
                .filter(|path| path.join("index.html").is_file())
        })
}

fn resolve_binary_dir<R: Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    std::env::var("AUTVID_SIDECAR_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            app.path()
                .resolve("binaries", tauri::path::BaseDirectory::Resource)
                .ok()
                .filter(|path| path.is_dir())
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(PathBuf::from))
                .filter(|path| path.is_dir())
        })
        .or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .canonicalize()
                .ok()
                .filter(|path| path.is_dir())
        })
}

#[tauri::command]
async fn desktop_open_in_browser(url: String) -> Result<(), String> {
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|err| err.to_string())
}

fn main() {
    let data_dir = resolve_data_dir("Autonomi Video Management")
        .expect("could not resolve Autonomi Video Management data directory");
    let state = Arc::new(DesktopState {
        data_dir,
        closing: AtomicBool::new(false),
        running: Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            desktop_setup_status,
            desktop_save_setup,
            desktop_start_stack,
            desktop_open_in_browser
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle().clone();
                let state = app.state::<Arc<DesktopState>>();
                if state.closing.swap(true, Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                let window = window.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<Arc<DesktopState>>();
                    let mut running = state.running.lock().await;
                    if let Some(stack) = running.take() {
                        stack.shutdown().await;
                    }
                    if let Err(err) = window.close() {
                        eprintln!("error closing window after sidecar shutdown: {err}");
                    }
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Autonomi Video Management");
}
