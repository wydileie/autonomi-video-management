import axios, {
  type AxiosError,
  type AxiosProgressEvent,
  type InternalAxiosRequestConfig,
} from "axios";

import { AUTH_STORAGE_KEY } from "../constants";
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

const API = API_BASE_URL;
const AUTH_ENDPOINTS = ["/auth/login", "/auth/refresh", "/auth/logout"];
const TRANSIENT_RETRY_DELAYS_MS = [150, 350];
const READ_METHODS = new Set(["get", "head", "options"]);

type RetryableRequestConfig = InternalAxiosRequestConfig & {
  _authRetry?: boolean;
  _transientRetryCount?: number;
};

type AuthRefreshListener = (auth: AuthState | null) => void;

const authRefreshListeners = new Set<AuthRefreshListener>();
let refreshPromise: Promise<AuthState> | null = null;

export function authHeaders(token?: string): Record<string, string> {
  return token ? { Authorization: `Bearer ${token}` } : {};
}

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

function setStoredToken(token: string) {
  if (typeof window !== "undefined") {
    window.localStorage.setItem(AUTH_STORAGE_KEY, token);
  }
}

function clearStoredToken() {
  if (typeof window !== "undefined") {
    window.localStorage.removeItem(AUTH_STORAGE_KEY);
  }
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

const API_PATH_PREFIX = pathFromUrl(API).replace(/\/+$/, "");

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

function setBearerHeader(config: RetryableRequestConfig, token: string) {
  const headers = config.headers as
    | (Record<string, unknown> & { set?: (name: string, value: string) => void })
    | undefined;
  if (headers && typeof headers.set === "function") {
    headers.set("Authorization", `Bearer ${token}`);
    return;
  }
  config.headers = {
    ...(headers ?? {}),
    ...authHeaders(token),
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

function installReliabilityInterceptor() {
  axios.interceptors?.response?.use?.(
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
          const auth = await sharedRefresh();
          setBearerHeader(config, auth.access_token);
          return axios.request(config);
        } catch (refreshError) {
          clearStoredToken();
          return Promise.reject(refreshError);
        }
      }

      if (config && canRetryTransient(error, config)) {
        const retryCount = config._transientRetryCount ?? 0;
        config._transientRetryCount = retryCount + 1;
        await delay(TRANSIENT_RETRY_DELAYS_MS[retryCount]);
        return axios.request(config);
      }

      return Promise.reject(error);
    },
  );
}

installReliabilityInterceptor();

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
  const res = await axios.post<AuthState>(`${API}/auth/login`, credentials, { withCredentials: true });
  setStoredToken(res.data.access_token);
  return res.data;
}

async function performRefreshAdmin(): Promise<AuthState> {
  try {
    const res = await axios.post<AuthState>(`${API}/auth/refresh`, null, { withCredentials: true });
    setStoredToken(res.data.access_token);
    notifyAuthRefresh(res.data);
    return res.data;
  } catch (err) {
    clearStoredToken();
    notifyAuthRefresh(null);
    throw err;
  }
}

export async function refreshAdmin(): Promise<AuthState> {
  return sharedRefresh();
}

export async function logoutAdmin() {
  try {
    await axios.post(`${API}/auth/logout`, null, { withCredentials: true });
  } finally {
    clearStoredToken();
    notifyAuthRefresh(null);
  }
}

export async function getCurrentUser(token: string): Promise<CurrentUser> {
  const res = await axios.get<CurrentUser>(`${API}/auth/me`, { headers: authHeaders(token) });
  return res.data;
}

export async function listVideos({
  admin = false,
  token = "",
}: { admin?: boolean; token?: string } = {}): Promise<VideoSummary[]> {
  const res = await axios.get<VideoSummary[]>(`${API}${admin ? "/admin" : ""}/videos`, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function getVideoDetails({
  admin = false,
  token = "",
  videoId,
}: { admin?: boolean; token?: string; videoId: string }): Promise<VideoDetail> {
  const res = await axios.get<VideoDetail>(`${API}${admin ? "/admin" : ""}/videos/${videoId}`, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function requestUploadQuote(
  token: string,
  quoteRequest: UploadQuoteRequest,
  signal: AbortSignal,
): Promise<UploadQuote> {
  const res = await axios.post<UploadQuote>(`${API}/videos/upload/quote`, quoteRequest, {
    headers: authHeaders(token),
    signal,
  });
  return res.data;
}

export async function uploadVideo(
  token: string,
  formData: FormData,
  onUploadProgress: (progressEvent: AxiosProgressEvent) => void,
): Promise<VideoSummary> {
  const res = await axios.post<VideoSummary>(`${API}/videos/upload`, formData, {
    headers: { "Content-Type": "multipart/form-data", ...authHeaders(token) },
    onUploadProgress,
  });
  return res.data;
}

export async function approveVideoUpload(token: string, videoId: string): Promise<VideoDetail> {
  const res = await axios.post<VideoDetail>(`${API}/admin/videos/${videoId}/approve`, null, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function deleteVideoRecord(token: string, videoId: string): Promise<void> {
  await axios.delete(`${API}/admin/videos/${videoId}`, { headers: authHeaders(token) });
}

export async function updateVideoVisibility(
  token: string,
  videoId: string,
  next: VisibilityUpdate,
): Promise<VideoDetail> {
  const res = await axios.patch<VideoDetail>(`${API}/admin/videos/${videoId}/visibility`, next, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function updateVideoPublication(
  token: string,
  videoId: string,
  isPublic: boolean,
): Promise<VideoDetail> {
  const res = await axios.patch<VideoDetail>(
    `${API}/admin/videos/${videoId}/publication`,
    { is_public: isPublic },
    { headers: authHeaders(token) },
  );
  return res.data;
}
