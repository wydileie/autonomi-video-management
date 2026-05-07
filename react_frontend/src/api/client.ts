import axios, {
  type AxiosError,
  type AxiosInstance,
  type AxiosProgressEvent,
  type InternalAxiosRequestConfig,
} from "axios";

import { API_BASE_URL } from "../runtimeConfig";
import type {
  AuthState,
  CurrentUser,
  LoginCredentials,
  UploadQuote,
  UploadQuoteRequest,
  VideoDetail,
  VideoSummary,
  VisibilityUpdate,
} from "../types";

const AUTH_ENDPOINTS = ["/auth/login", "/auth/refresh", "/auth/logout"];
const TRANSIENT_RETRY_DELAYS_MS = [150, 350];
const READ_METHODS = new Set(["get", "head", "options"]);
const CSRF_COOKIE = "autvid_csrf";
const CSRF_HEADER = "X-CSRF-Token";

type RetryableRequestConfig = InternalAxiosRequestConfig & {
  _authRetry?: boolean;
  _transientRetryCount?: number;
};

type AuthRefreshListener = (auth: AuthState | null) => void;

const authRefreshListeners = new Set<AuthRefreshListener>();
let refreshPromise: Promise<AuthState> | null = null;

export const api: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  timeout: 60_000,
  withCredentials: true,
});

export function subscribeAuthRefresh(listener: AuthRefreshListener): () => void {
  authRefreshListeners.add(listener);
  return () => {
    authRefreshListeners.delete(listener);
  };
}

function notifyAuthRefresh(auth: AuthState | null) {
  authRefreshListeners.forEach((listener) => {
    try {
      listener(auth);
    } catch {
      // Listener failures should not break API retries.
    }
  });
}

function delay(ms: number) {
  return new Promise((resolve) => {
    globalThis.setTimeout(resolve, ms);
  });
}

function pathFromUrl(url = "", baseURL?: string): string {
  const fallbackBase = typeof window !== "undefined" ? window.location.origin : "http://localhost";
  try {
    const parsed = new URL(url, baseURL || fallbackBase);
    return parsed.pathname.replace(/\/+$/, "") || "/";
  } catch {
    return url.split("?")[0].replace(/\/+$/, "") || "/";
  }
}

const API_PATH_PREFIX = pathFromUrl(API_BASE_URL).replace(/\/+$/, "");

function requestPath(config: RetryableRequestConfig): string {
  return pathFromUrl(config.url, config.baseURL);
}

function isApiEndpoint(config: RetryableRequestConfig, suffix: string): boolean {
  const path = requestPath(config);
  return path === suffix || path === `${API_PATH_PREFIX}${suffix}`;
}

function isAuthEndpoint(config: RetryableRequestConfig): boolean {
  return AUTH_ENDPOINTS.some((endpoint) => isApiEndpoint(config, endpoint));
}

function isCsrfExemptAuthEndpoint(config: RetryableRequestConfig): boolean {
  return isApiEndpoint(config, "/auth/login") || isApiEndpoint(config, "/auth/refresh");
}

function requestMethod(config: RetryableRequestConfig): string {
  return (config.method || "get").toLowerCase();
}

function isUploadQuoteRequest(config: RetryableRequestConfig): boolean {
  return requestMethod(config) === "post" && isApiEndpoint(config, "/videos/upload/quote");
}

function isTransientError(error: AxiosError): boolean {
  if (!error.response) return true;
  const status = error.response.status;
  return status >= 500 && status < 600;
}

function canRetryTransient(error: AxiosError, config: RetryableRequestConfig): boolean {
  if (!isTransientError(error) || isRequestCanceled(error)) return false;
  const retryCount = config._transientRetryCount ?? 0;
  if (retryCount >= TRANSIENT_RETRY_DELAYS_MS.length) return false;
  const method = requestMethod(config);
  return READ_METHODS.has(method) || isUploadQuoteRequest(config);
}

function cookieValue(name: string): string {
  if (typeof document === "undefined") return "";
  const prefix = `${name}=`;
  return document.cookie
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith(prefix))
    ?.slice(prefix.length) || "";
}

export function hasCsrfCookie(): boolean {
  return !!cookieValue(CSRF_COOKIE);
}

function setHeader(config: RetryableRequestConfig, name: string, value: string) {
  const headers = config.headers as
    | (Record<string, unknown> & { set?: (headerName: string, headerValue: string) => void })
    | undefined;
  if (headers && typeof headers.set === "function") {
    headers.set(name, value);
    return;
  }
  config.headers = {
    ...(headers ?? {}),
    [name]: value,
  } as RetryableRequestConfig["headers"];
}

function sharedRefresh(): Promise<AuthState> {
  if (!refreshPromise) {
    refreshPromise = performRefreshAdmin().finally(() => {
      refreshPromise = null;
    });
  }
  return refreshPromise;
}

function installInterceptors() {
  api.interceptors.request.use((config) => {
    const retryableConfig = config as RetryableRequestConfig;
    const method = requestMethod(retryableConfig);
    if (!READ_METHODS.has(method) && !isCsrfExemptAuthEndpoint(retryableConfig)) {
      const csrf = cookieValue(CSRF_COOKIE);
      if (csrf) setHeader(retryableConfig, CSRF_HEADER, csrf);
    }
    return retryableConfig;
  });

  api.interceptors.response.use(
    (response) => response,
    async (error: AxiosError) => {
      const config = error.config as RetryableRequestConfig | undefined;

      if (
        config
        && error.response?.status === 401
        && !config._authRetry
        && !isAuthEndpoint(config)
      ) {
        config._authRetry = true;
        try {
          await sharedRefresh();
          return api.request(config);
        } catch (refreshError) {
          notifyAuthRefresh(null);
          return Promise.reject(refreshError);
        }
      }

      if (config && canRetryTransient(error, config)) {
        const retryCount = config._transientRetryCount ?? 0;
        config._transientRetryCount = retryCount + 1;
        await delay(TRANSIENT_RETRY_DELAYS_MS[retryCount]);
        return api.request(config);
      }

      return Promise.reject(error);
    },
  );
}

installInterceptors();

type RequestErrorShape = {
  message?: string;
  response?: {
    data?: {
      detail?: string;
    };
  };
};

export function requestErrorMessage(err: unknown, fallback: string): string {
  if (err && typeof err === "object") {
    const requestError = err as RequestErrorShape;
    return requestError.response?.data?.detail || requestError.message || fallback;
  }
  return fallback;
}

export function isRequestCanceled(err: unknown): boolean {
  return axios.isCancel(err) || (err instanceof Error && err.name === "CanceledError");
}

export async function loginAdmin(credentials: LoginCredentials): Promise<AuthState> {
  const res = await api.post<AuthState>("/auth/login", credentials);
  return res.data;
}

async function performRefreshAdmin(): Promise<AuthState> {
  try {
    const res = await api.post<AuthState>("/auth/refresh", null);
    notifyAuthRefresh(res.data);
    return res.data;
  } catch (err) {
    notifyAuthRefresh(null);
    throw err;
  }
}

export async function refreshAdmin(): Promise<AuthState> {
  return sharedRefresh();
}

export async function logoutAdmin() {
  try {
    await api.post("/auth/logout", null);
  } finally {
    notifyAuthRefresh(null);
  }
}

export async function getCurrentUser(): Promise<CurrentUser> {
  const res = await api.get<CurrentUser>("/auth/me");
  return res.data;
}

export async function listVideos({ admin = false }: { admin?: boolean } = {}): Promise<VideoSummary[]> {
  const res = await api.get<VideoSummary[]>(`${admin ? "/admin" : ""}/videos`);
  return res.data;
}

export async function getVideoDetails({
  admin = false,
  videoId,
}: { admin?: boolean; videoId: string }): Promise<VideoDetail> {
  const res = await api.get<VideoDetail>(`${admin ? "/admin" : ""}/videos/${videoId}`);
  return res.data;
}

export async function requestUploadQuote(
  quoteRequest: UploadQuoteRequest,
  signal: AbortSignal,
): Promise<UploadQuote> {
  const res = await api.post<UploadQuote>("/videos/upload/quote", quoteRequest, { signal });
  return res.data;
}

export async function uploadVideo(
  formData: FormData,
  onUploadProgress: (progressEvent: AxiosProgressEvent) => void,
): Promise<VideoSummary> {
  const res = await api.post<VideoSummary>("/videos/upload", formData, {
    headers: { "Content-Type": "multipart/form-data" },
    onUploadProgress,
  });
  return res.data;
}

export async function approveVideoUpload(videoId: string): Promise<VideoDetail> {
  const res = await api.post<VideoDetail>(`/admin/videos/${videoId}/approve`, null);
  return res.data;
}

export async function deleteVideoRecord(videoId: string): Promise<void> {
  await api.delete(`/admin/videos/${videoId}`);
}

export async function updateVideoVisibility(
  videoId: string,
  next: VisibilityUpdate,
): Promise<VideoDetail> {
  const res = await api.patch<VideoDetail>(`/admin/videos/${videoId}/visibility`, next);
  return res.data;
}

export async function updateVideoPublication(
  videoId: string,
  isPublic: boolean,
): Promise<VideoDetail> {
  const res = await api.patch<VideoDetail>(
    `/admin/videos/${videoId}/publication`,
    { is_public: isPublic },
  );
  return res.data;
}
