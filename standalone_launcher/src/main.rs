use std::{env, path::PathBuf};

use anyhow::anyhow;
use launcher_core::{launch_stack, LaunchOptions, NetworkMode};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

struct Options {
    mode: NetworkMode,
    no_open: bool,
    require_setup: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let options = parse_options()?;
    let stack = launch_stack(LaunchOptions {
        mode: options.mode,
        app_name: "Autonomi Video Management".to_string(),
        data_dir: env::var("AUTVID_DATA_DIR").ok().map(PathBuf::from),
        binary_dir: None,
        frontend_dir: env::var("AUTVID_FRONTEND_DIR").ok().map(PathBuf::from),
        require_setup: options.require_setup,
        open_browser: !options.no_open,
    })
    .await?;

    tokio::signal::ctrl_c().await?;
    stack.shutdown().await;
    Ok(())
}

fn parse_options() -> anyhow::Result<Options> {
    let mut mode = NetworkMode::Configured;
    let mut no_open = false;
    let mut require_setup = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--configured-network" => mode = NetworkMode::Configured,
            "--local-devnet" => mode = NetworkMode::LocalDevnet,
            "--require-setup" => require_setup = true,
            "--mode" => {
                let value = args.next().ok_or_else(|| anyhow!("--mode needs a value"))?;
                mode = match value.as_str() {
                    "configured" | "configured-network" => NetworkMode::Configured,
                    "local" | "local-devnet" | "devnet" => NetworkMode::LocalDevnet,
                    _ => return Err(anyhow!("unknown launcher mode: {value}")),
                };
            }
            "--no-open" => no_open = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => return Err(anyhow!("unknown argument: {arg}")),
        }
    }
    Ok(Options {
        mode,
        no_open,
        require_setup,
    })
}

fn print_help() {
    println!(
        "Usage: autvid_launcher [--mode configured|local-devnet] [--no-open] [--require-setup]"
    );
}
