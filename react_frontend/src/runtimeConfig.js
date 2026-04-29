const DEFAULT_API_BASE_URL = "/api";
const DEFAULT_STREAM_BASE_URL = "/stream";

function runtimeBrowserConfig() {
  if (typeof window === "undefined") return {};
  const config = window.__AUTONOMI_VIDEO_CONFIG__;
  return config && typeof config === "object" ? config : {};
}

function firstString(...values) {
  return values.find((value) => typeof value === "string" && value.trim());
}

function normalizeBaseUrl(value) {
  const trimmed = value.trim();
  return trimmed === "/" ? trimmed : trimmed.replace(/\/+$/, "");
}

export function resolveRuntimeConfig({
  browserConfig = runtimeBrowserConfig(),
  env = import.meta.env,
} = {}) {
  const apiBaseUrl = firstString(
    browserConfig.apiBaseUrl,
    browserConfig.apiUrl,
    browserConfig.REACT_APP_API_URL,
    browserConfig.VITE_API_URL,
    env.REACT_APP_API_URL,
    env.VITE_API_URL,
    DEFAULT_API_BASE_URL,
  );
  const streamBaseUrl = firstString(
    browserConfig.streamBaseUrl,
    browserConfig.streamUrl,
    browserConfig.REACT_APP_STREAM_URL,
    browserConfig.VITE_STREAM_URL,
    env.REACT_APP_STREAM_URL,
    env.VITE_STREAM_URL,
    DEFAULT_STREAM_BASE_URL,
  );

  return {
    apiBaseUrl: normalizeBaseUrl(apiBaseUrl),
    streamBaseUrl: normalizeBaseUrl(streamBaseUrl),
  };
}

export const { apiBaseUrl: API_BASE_URL, streamBaseUrl: STREAM_BASE_URL } = resolveRuntimeConfig();
