import axios, { type AxiosProgressEvent } from "axios";

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

export function authHeaders(token?: string): Record<string, string> {
  return token ? { Authorization: `Bearer ${token}` } : {};
}

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
  window.localStorage.setItem(AUTH_STORAGE_KEY, res.data.access_token);
  return res.data;
}

export async function refreshAdmin(): Promise<AuthState> {
  const res = await axios.post<AuthState>(`${API}/auth/refresh`, null, { withCredentials: true });
  window.localStorage.setItem(AUTH_STORAGE_KEY, res.data.access_token);
  return res.data;
}

export async function logoutAdmin() {
  try {
    await axios.post(`${API}/auth/logout`, null, { withCredentials: true });
  } finally {
    window.localStorage.removeItem(AUTH_STORAGE_KEY);
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
