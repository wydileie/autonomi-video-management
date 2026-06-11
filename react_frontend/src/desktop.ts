import type { SetupStatus } from "./types";

type DesktopSetupRequest = {
  adminUsername: string;
  adminPassword: string;
  walletKey?: string;
  walletKeyFile?: string;
};

type LaunchResponse = {
  url: string;
};

type TauriCore = {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
};

export function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function tauriCore(): Promise<TauriCore> {
  return await import("@tauri-apps/api/core");
}

export async function desktopSetupStatus(): Promise<SetupStatus> {
  const { invoke } = await tauriCore();
  return invoke<SetupStatus>("desktop_setup_status");
}

export async function desktopSaveSetup(setup: DesktopSetupRequest): Promise<SetupStatus> {
  const { invoke } = await tauriCore();
  return invoke<SetupStatus>("desktop_save_setup", { setup });
}

export async function desktopStartStack(): Promise<LaunchResponse> {
  const { invoke } = await tauriCore();
  return invoke<LaunchResponse>("desktop_start_stack");
}

export async function desktopOpenInBrowser(url: string): Promise<void> {
  const { invoke } = await tauriCore();
  await invoke("desktop_open_in_browser", { url });
}
