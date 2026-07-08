import { describe, expect, test } from "vitest";

import { isLaunchedLocalAppLocation } from "./desktop";

describe("isLaunchedLocalAppLocation", () => {
  test("detects the launcher-served desktop app on loopback HTTP", () => {
    expect(
      isLaunchedLocalAppLocation({
        hostname: "127.0.0.1",
        port: "8080",
        protocol: "http:",
      }),
    ).toBe(true);
    expect(
      isLaunchedLocalAppLocation({
        hostname: "localhost",
        port: "49152",
        protocol: "http:",
      }),
    ).toBe(true);
  });

  test("keeps Tauri dev server URLs eligible for setup IPC", () => {
    expect(
      isLaunchedLocalAppLocation({
        hostname: "127.0.0.1",
        port: "5173",
        protocol: "http:",
      }),
    ).toBe(false);
  });

  test("does not treat Tauri asset origins as launched local app URLs", () => {
    expect(
      isLaunchedLocalAppLocation({
        hostname: "tauri.localhost",
        port: "",
        protocol: "http:",
      }),
    ).toBe(false);
    expect(
      isLaunchedLocalAppLocation({
        hostname: "localhost",
        port: "",
        protocol: "tauri:",
      }),
    ).toBe(false);
  });

  test("does not treat external browser URLs as launched local app URLs", () => {
    expect(
      isLaunchedLocalAppLocation({
        hostname: "example.com",
        port: "",
        protocol: "https:",
      }),
    ).toBe(false);
  });
});
