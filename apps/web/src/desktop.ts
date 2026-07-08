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
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return false;
  }

  const { hostname, port, protocol } = window.location;
  const isLaunchedLocalApp = isLaunchedLocalAppLocation({ hostname, port, protocol });

  return !isLaunchedLocalApp;
}

export function isLaunchedLocalAppLocation(
  location: Pick<Location, "hostname" | "port" | "protocol">,
): boolean {
  const { hostname, port, protocol } = location;
  const isLocalHttp = protocol === "http:" || protocol === "https:";
  const isLoopbackHost =
    hostname === "127.0.0.1" || hostname === "localhost" || hostname === "[::1]";

  // Tauri dev loads Vite on 5173 and still needs setup IPC; the launched app UI does not.
  return isLocalHttp && isLoopbackHost && port !== "5173";
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
