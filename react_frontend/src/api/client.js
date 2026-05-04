import axios from "axios";

import { API_BASE_URL } from "../runtimeConfig";

const API = API_BASE_URL;

export function authHeaders(token) {
  return token ? { Authorization: `Bearer ${token}` } : {};
}

export function requestErrorMessage(err, fallback) {
  return err?.response?.data?.detail || err.message || fallback;
}

export function isRequestCanceled(err) {
  return axios.isCancel(err) || err.name === "CanceledError";
}

export async function loginAdmin(credentials) {
  const res = await axios.post(`${API}/auth/login`, credentials);
  return res.data;
}

export async function getCurrentUser(token) {
  const res = await axios.get(`${API}/auth/me`, { headers: authHeaders(token) });
  return res.data;
}

export async function listVideos({ admin = false, token = "" } = {}) {
  const res = await axios.get(`${API}${admin ? "/admin" : ""}/videos`, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function getVideoDetails({ admin = false, token = "", videoId }) {
  const res = await axios.get(`${API}${admin ? "/admin" : ""}/videos/${videoId}`, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function requestUploadQuote(token, quoteRequest, signal) {
  const res = await axios.post(`${API}/videos/upload/quote`, quoteRequest, {
    headers: authHeaders(token),
    signal,
  });
  return res.data;
}

export async function uploadVideo(token, formData, onUploadProgress) {
  const res = await axios.post(`${API}/videos/upload`, formData, {
    headers: { "Content-Type": "multipart/form-data", ...authHeaders(token) },
    onUploadProgress,
  });
  return res.data;
}

export async function approveVideoUpload(token, videoId) {
  const res = await axios.post(`${API}/admin/videos/${videoId}/approve`, null, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function deleteVideoRecord(token, videoId) {
  await axios.delete(`${API}/admin/videos/${videoId}`, { headers: authHeaders(token) });
}

export async function updateVideoVisibility(token, videoId, next) {
  const res = await axios.patch(`${API}/admin/videos/${videoId}/visibility`, next, {
    headers: authHeaders(token),
  });
  return res.data;
}

export async function updateVideoPublication(token, videoId, isPublic) {
  const res = await axios.patch(`${API}/admin/videos/${videoId}/publication`, { is_public: isPublic }, {
    headers: authHeaders(token),
  });
  return res.data;
}
