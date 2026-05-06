import { describe, expect, test } from "vitest";
import { resolveRuntimeConfig } from "./runtimeConfig";

describe("resolveRuntimeConfig", () => {
  test("uses runtime browser config before build-time env defaults", () => {
    expect(resolveRuntimeConfig({
      browserConfig: {
        apiBaseUrl: "http://native-host/api/",
        streamBaseUrl: "http://native-host/stream/",
      },
      env: {
        REACT_APP_API_URL: "http://env/api",
        REACT_APP_STREAM_URL: "http://env/stream",
      },
    })).toEqual({
      apiBaseUrl: "http://native-host/api",
      streamBaseUrl: "http://native-host/stream",
    });
  });

  test("falls back to build-time env values before relative defaults", () => {
    expect(resolveRuntimeConfig({
      browserConfig: {},
      env: {
        REACT_APP_API_URL: "http://env/api",
        REACT_APP_STREAM_URL: "http://env/stream",
      },
    })).toEqual({
      apiBaseUrl: "http://env/api",
      streamBaseUrl: "http://env/stream",
    });
  });

  test("keeps the current relative defaults when no config is provided", () => {
    expect(resolveRuntimeConfig({ browserConfig: {}, env: {} })).toEqual({
      apiBaseUrl: "/api",
      streamBaseUrl: "/stream",
    });
  });
});
