import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { vi } from "vitest";
import axios from "axios";
import Hls from "hls.js";
import App from "./App";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("axios", () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    patch: vi.fn(),
    delete: vi.fn(),
    isCancel: vi.fn(() => false),
  },
}));

vi.mock("hls.js", () => {
  const Hls = vi.fn().mockImplementation(() => ({
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
  }));
  Hls.Events = { ERROR: "hlsError", MANIFEST_PARSED: "manifestParsed" };
  Hls.isSupported = vi.fn(() => false);
  return { default: Hls };
});

const AUTH_STORAGE_KEY = "autvid_admin_token";
const flushPromises = () => act(async () => {
  await Promise.resolve();
  await Promise.resolve();
});

let container;
let root;

function text() {
  return container.textContent;
}

function findButton(label) {
  const matcher = label instanceof RegExp ? label : new RegExp(label, "i");
  const button = Array.from(container.querySelectorAll("button"))
    .find((candidate) => matcher.test(candidate.textContent));
  if (!button) throw new Error(`Unable to find button matching ${matcher}`);
  return button;
}

function setInputValue(input, value) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  act(() => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function click(element) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flushPromises();
}

async function mouseLeave(element) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("mouseout", {
      bubbles: true,
      relatedTarget: document.body,
    }));
  });
  await flushPromises();
}

async function renderApp() {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root.render(<App />);
  });
  await flushPromises();
}

async function waitFor(assertion) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < 1500) {
    try {
      return assertion();
    } catch (error) {
      lastError = error;
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 10));
      });
    }
  }
  throw lastError;
}

function setupGetRoutes({
  publicVideos = [],
  adminVideos = [],
  details = {},
  currentUser = { username: "admin" },
} = {}) {
  axios.get.mockImplementation((url, config = {}) => {
    if (url === "/api/auth/me") {
      return Promise.resolve({ data: currentUser });
    }
    if (url === "/api/videos") {
      return Promise.resolve({ data: publicVideos });
    }
    if (url === "/api/admin/videos") {
      return Promise.resolve({ data: adminVideos });
    }
    const detail = details[url];
    if (detail) {
      return Promise.resolve({ data: detail });
    }
    return Promise.reject(new Error(`Unexpected GET ${url} ${JSON.stringify(config)}`));
  });
}

beforeEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
  Hls.isSupported.mockReturnValue(false);
  Hls.mockImplementation(() => ({
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
  }));
  window.localStorage.clear();
  if (!URL.createObjectURL) {
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: vi.fn(),
    });
  }
  if (!URL.revokeObjectURL) {
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: vi.fn(),
    });
  }
  vi.spyOn(window, "confirm").mockReturnValue(true);
  vi.spyOn(window, "alert").mockImplementation(() => {});
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:video");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
});

afterEach(async () => {
  if (root) {
    await act(async () => {
      root.unmount();
    });
  }
  if (container) {
    container.remove();
  }
  vi.useRealTimers();
  root = null;
  container = null;
  vi.restoreAllMocks();
});

test("logs in, stores the admin token, and sends bearer auth on admin requests", async () => {
  setupGetRoutes();
  axios.post.mockImplementation((url, body) => {
    if (url === "/api/auth/login") {
      expect(body).toEqual({ username: "admin", password: "secret" });
      return Promise.resolve({ data: { access_token: "token-123", username: "admin" } });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Login"));
  setInputValue(container.querySelector('input[type="password"]'), "secret");
  await click(findButton("Sign in"));

  await waitFor(() => {
    expect(window.localStorage.getItem(AUTH_STORAGE_KEY)).toBe("token-123");
    expect(text()).toContain("No videos yet. Upload one to build your first stream.");
  });
  expect(text()).toContain("Manage");
  expect(text()).toContain("Upload");
  expect(text()).toContain("Logout");
  expect(axios.get).toHaveBeenCalledWith(
    "/api/auth/me",
    { headers: { Authorization: "Bearer token-123" } },
  );
  expect(axios.get).toHaveBeenCalledWith(
    "/api/admin/videos",
    { headers: { Authorization: "Bearer token-123" } },
  );
});

test("shows an upload quote after local video metadata is available", async () => {
  vi.useFakeTimers();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  setupGetRoutes();

  const realCreateElement = document.createElement.bind(document);
  vi.spyOn(document, "createElement").mockImplementation((tagName, options) => {
    const element = realCreateElement(tagName, options);
    if (tagName === "video") {
      Object.defineProperties(element, {
        duration: { configurable: true, value: 64 },
        videoHeight: { configurable: true, value: 1080 },
        videoWidth: { configurable: true, value: 1920 },
        src: {
          configurable: true,
          get: () => "blob:video",
          set: () => {
            if (element.onloadedmetadata) element.onloadedmetadata();
          },
        },
      });
    }
    return element;
  });

  axios.post.mockImplementation((url, body, config) => {
    if (url === "/api/videos/upload/quote") {
      expect(body).toEqual({
        duration_seconds: 64,
        resolutions: ["1080p", "720p", "480p", "360p"],
        source_height: 1080,
        source_width: 1920,
      });
      expect(config.headers).toEqual({ Authorization: "Bearer stored-token" });
      return Promise.resolve({
        data: {
          estimated_bytes: 5120,
          estimated_gas_cost_wei: "12345",
          payment_mode: "single",
          sampled: false,
          segment_count: 3,
          storage_cost_atto: "2000000000000000000",
        },
      });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Upload"));

  const fileInput = container.querySelector('input[type="file"]');
  const file = new File(["fake video"], "launch.mp4", { type: "video/mp4" });
  await act(async () => {
    Object.defineProperty(fileInput, "files", { configurable: true, value: [file] });
    fileInput.dispatchEvent(new Event("change", { bubbles: true }));
  });

  expect(text()).toContain("launch.mp4");
  expect(text()).toContain("1920 x 1080");

  await act(async () => {
    vi.advanceTimersByTime(260);
  });
  await flushPromises();

  expect(text()).toContain("2 ANT");
  expect(text()).toContain("5.0 KB across 3 HLS segments and metadata");
  expect(text()).toContain("12,345 wei");
});

test("approves an awaiting upload and deletes the video through admin controls", async () => {
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  const adminVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Needs operator review",
    id: "vid-approval",
    original_filename: "raw-upload.mov",
    status: "awaiting_approval",
    title: "Needs approval",
  };
  const awaitingDetail = {
    ...adminVideo,
    approval_expires_at: "2026-04-29T12:00:00Z",
    final_quote: {
      actual_media_bytes: 2048,
      estimated_gas_cost_wei: "7000",
      metadata_bytes: 512,
      payment_mode: "single",
      segment_count: 2,
      storage_cost_atto: "1500000000000000000",
    },
    manifest_address: null,
    show_manifest_address: false,
    show_original_filename: false,
    variants: [],
  };
  setupGetRoutes({
    adminVideos: [adminVideo],
    details: { "/api/admin/videos/vid-approval": awaitingDetail },
  });

  let resolveApproval;
  axios.post.mockImplementation((url, body, config) => {
    if (url === "/api/admin/videos/vid-approval/approve") {
      expect(body).toBeNull();
      expect(config.headers).toEqual({ Authorization: "Bearer stored-token" });
      return new Promise((resolve) => {
        resolveApproval = resolve;
      });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });
  axios.delete.mockResolvedValue({ data: {} });

  await renderApp();
  await click(findButton("Manage"));
  await waitFor(() => expect(text()).toContain("Needs approval"));
  await click(findButton("Needs approval"));

  expect(text()).toContain("Final Autonomi quote");
  expect(text()).toContain("Approve upload");

  await act(async () => {
    findButton("Approve upload").dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(text()).toContain("Approving...");

  await act(async () => {
    resolveApproval({
      data: {
        ...awaitingDetail,
        final_quote: null,
        manifest_address: "0xmanifest",
        status: "published",
      },
    });
  });
  await flushPromises();

  await click(findButton("Delete"));
  expect(window.confirm).toHaveBeenCalledWith(
    "Delete this video record and remove it from the network catalog?",
  );
  expect(axios.delete).toHaveBeenCalledWith(
    "/api/admin/videos/vid-approval",
    { headers: { Authorization: "Bearer stored-token" } },
  );
  expect(text()).not.toContain("Needs approval");
});

test("keeps public catalog metadata redacted when filename and manifest are hidden", async () => {
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-1",
    original_filename: "",
    status: "published",
    title: "Public stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-1": {
        ...publicVideo,
        manifest_address: null,
        variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Public stream"));

  expect(text()).toContain("Public description only");
  expect(text()).not.toContain("private-source.mov");
  expect(text()).not.toContain("0xprivate");

  await click(findButton("Public stream"));

  expect(text()).toContain("720p");
  expect(text()).not.toContain("4 segments");
  expect(text()).not.toContain("Manifest hidden or pending");
  expect(text()).not.toContain("Delete");
  expect(text()).not.toContain("Publish manifest address");
});

test("shows a playback error when HLS segment loading fails", async () => {
  Hls.isSupported.mockReturnValue(true);
  const hlsInstance = {
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
  };
  Hls.mockImplementation(() => hlsInstance);
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-err",
    original_filename: "",
    status: "ready",
    title: "Broken stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-err": {
        ...publicVideo,
        manifest_address: null,
        variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Broken stream"));
  await click(findButton("Broken stream"));

  const errorHandler = hlsInstance.on.mock.calls
    .find(([eventName]) => eventName === Hls.Events.ERROR)[1];
  await act(async () => {
    errorHandler(Hls.Events.ERROR, { fatal: true });
  });

  expect(text()).toContain("Playback failed because the video segments could not be loaded.");
});

test("passes the current playback position to hls.js when changing resolution", async () => {
  Hls.isSupported.mockReturnValue(true);
  const hlsInstances = [];
  Hls.mockImplementation(() => {
    const hlsInstance = {
      attachMedia: vi.fn((media) => {
        hlsInstance.media = media;
      }),
      destroy: vi.fn(() => {
        if (hlsInstance.media) hlsInstance.media.currentTime = 0;
      }),
      handlers: {},
      loadSource: vi.fn(),
      media: null,
      on: vi.fn((eventName, handler) => {
        hlsInstance.handlers[eventName] = handler;
      }),
    };
    hlsInstances.push(hlsInstance);
    return hlsInstance;
  });
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-resume",
    original_filename: "",
    status: "published",
    title: "Resume quality stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-resume": {
        ...publicVideo,
        manifest_address: null,
        variants: [
          { id: "variant-720", resolution: "720p", segment_count: 4 },
          { id: "variant-480", resolution: "480p", segment_count: 4 },
        ],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Resume quality stream"));
  await click(findButton("Resume quality stream"));

  const video = container.querySelector("video");
  Object.defineProperties(video, {
    currentTime: { configurable: true, writable: true, value: 42 },
    ended: { configurable: true, value: false },
    paused: { configurable: true, value: false },
    play: { configurable: true, value: vi.fn(() => Promise.resolve()) },
  });

  await click(container.querySelector(".quality-toggle"));
  await click(findButton("480p"));

  expect(hlsInstances).toHaveLength(2);
  expect(hlsInstances[0].destroy).toHaveBeenCalled();
  expect(Hls.mock.calls[1][0]).toMatchObject({ startPosition: 42 });
  expect(hlsInstances[1].loadSource).toHaveBeenCalledWith("/stream/pub-resume/480p/playlist.m3u8");
  expect(video.currentTime).toBe(0);

  await act(async () => {
    hlsInstances[1].handlers[Hls.Events.MANIFEST_PARSED]();
  });

  expect(video.currentTime).toBe(0);
  expect(video.play).toHaveBeenCalled();
});

test("waits for native HLS duration before seeking after a resolution change", async () => {
  Hls.isSupported.mockReturnValue(false);
  vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockImplementation((type) => (
    type === "application/vnd.apple.mpegurl" ? "maybe" : ""
  ));
  vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(function load() {
    this.currentTime = 0;
  });
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-native-resume",
    original_filename: "",
    status: "published",
    title: "Native resume stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-native-resume": {
        ...publicVideo,
        manifest_address: null,
        variants: [
          { id: "variant-720", resolution: "720p", segment_count: 20 },
          { id: "variant-480", resolution: "480p", segment_count: 20 },
        ],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Native resume stream"));
  await click(findButton("Native resume stream"));

  const video = container.querySelector("video");
  let currentTime = 7;
  let duration = 1;
  Object.defineProperties(video, {
    currentTime: {
      configurable: true,
      get: () => currentTime,
      set: (value) => {
        currentTime = value;
      },
    },
    duration: {
      configurable: true,
      get: () => duration,
    },
    ended: { configurable: true, value: false },
    paused: { configurable: true, value: false },
    play: { configurable: true, value: vi.fn(() => Promise.resolve()) },
  });

  await click(container.querySelector(".quality-toggle"));
  await click(findButton("480p"));

  expect(video.currentTime).toBe(0);

  await act(async () => {
    video.dispatchEvent(new Event("loadedmetadata"));
  });

  expect(video.currentTime).toBe(0);
  expect(video.play).not.toHaveBeenCalled();

  duration = 20;
  await act(async () => {
    video.dispatchEvent(new Event("durationchange"));
  });

  expect(video.currentTime).toBe(7);
  expect(video.play).toHaveBeenCalled();
});

test("closes the playback resolution menu when the pointer leaves it", async () => {
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-quality",
    original_filename: "",
    status: "published",
    title: "Quality stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-quality": {
        ...publicVideo,
        manifest_address: null,
        variants: [
          { id: "variant-720", resolution: "720p", segment_count: 4 },
          { id: "variant-480", resolution: "480p", segment_count: 4 },
        ],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Quality stream"));
  await click(findButton("Quality stream"));

  await click(container.querySelector(".quality-toggle"));
  expect(container.querySelector(".quality-menu")).not.toBeNull();

  await mouseLeave(container.querySelector(".player-quality"));
  expect(container.querySelector(".quality-menu")).toBeNull();
});

test("hides playback resolution controls after idle playback", async () => {
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-idle-quality",
    original_filename: "",
    status: "published",
    title: "Idle quality stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-idle-quality": {
        ...publicVideo,
        manifest_address: null,
        variants: [
          { id: "variant-720", resolution: "720p", segment_count: 4 },
          { id: "variant-480", resolution: "480p", segment_count: 4 },
        ],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Idle quality stream"));
  await click(findButton("Idle quality stream"));
  await click(container.querySelector(".quality-toggle"));

  const playerShell = container.querySelector(".player-shell");
  const video = container.querySelector("video");
  expect(playerShell.classList.contains("controls-active")).toBe(true);
  expect(container.querySelector(".quality-menu")).not.toBeNull();

  vi.useFakeTimers();
  Object.defineProperty(video, "paused", { configurable: true, value: false });
  await act(async () => {
    video.dispatchEvent(new Event("play", { bubbles: true }));
  });

  await act(async () => {
    vi.advanceTimersByTime(2200);
  });

  expect(playerShell.classList.contains("controls-active")).toBe(false);
  expect(container.querySelector(".quality-menu")).toBeNull();
});

test("uses the manifest-address stream route for admin preview before publishing", async () => {
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  Hls.isSupported.mockReturnValue(true);
  const hlsInstance = {
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
  };
  Hls.mockImplementation(() => hlsInstance);
  const adminVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Operators only",
    id: "admin-preview",
    is_public: false,
    original_filename: "source.mov",
    status: "ready",
    title: "Preview test",
  };
  setupGetRoutes({
    adminVideos: [adminVideo],
    details: {
      "/api/admin/videos/admin-preview": {
        ...adminVideo,
        manifest_address: "0xmanifest",
        show_manifest_address: false,
        show_original_filename: false,
        variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
      },
    },
  });

  await renderApp();
  await click(findButton("Manage"));
  await waitFor(() => expect(text()).toContain("Preview test"));
  await click(findButton("Preview test"));

  expect(hlsInstance.loadSource).toHaveBeenCalledWith(
    "/stream/manifest/0xmanifest/720p/playlist.m3u8",
  );
});

test("publishes and unpublishes ready videos from admin controls", async () => {
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  const adminVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Operators only",
    id: "admin-publication",
    is_public: false,
    original_filename: "source.mov",
    status: "ready",
    title: "Publication test",
  };
  let detail = {
    ...adminVideo,
    manifest_address: "0xmanifest",
    show_manifest_address: false,
    show_original_filename: false,
    variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
  };

  setupGetRoutes({
    adminVideos: [adminVideo],
    details: { "/api/admin/videos/admin-publication": detail },
  });
  axios.patch.mockImplementation((url, body, config) => {
    if (url === "/api/admin/videos/admin-publication/publication") {
      expect(config.headers).toEqual({ Authorization: "Bearer stored-token" });
      detail = { ...detail, is_public: body.is_public };
      return Promise.resolve({ data: detail });
    }
    return Promise.reject(new Error(`Unexpected PATCH ${url}`));
  });

  await renderApp();
  await click(findButton("Manage"));
  await waitFor(() => expect(text()).toContain("Publication test"));
  await click(findButton("Publication test"));

  await click(findButton(/^Publish$/));
  expect(axios.patch).toHaveBeenCalledWith(
    "/api/admin/videos/admin-publication/publication",
    { is_public: true },
    { headers: { Authorization: "Bearer stored-token" } },
  );
  expect(text()).toContain("Unpublish");

  await click(findButton("Unpublish"));
  expect(axios.patch).toHaveBeenCalledWith(
    "/api/admin/videos/admin-publication/publication",
    { is_public: false },
    { headers: { Authorization: "Bearer stored-token" } },
  );
  expect(text()).toContain("Publish");
});

test("shows a catalog load error when the initial library request fails", async () => {
  axios.get.mockRejectedValue(new Error("Catalog offline"));

  await renderApp();

  await waitFor(() => expect(text()).toContain("Catalog offline"));
  expect(text()).not.toContain("No videos are available yet.");
});

test("shows detail, delete, and visibility failures without removing the current row", async () => {
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  const adminVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Operators only",
    id: "admin-1",
    original_filename: "source.mov",
    status: "published",
    title: "Admin stream",
  };
  const detail = {
    ...adminVideo,
    manifest_address: "0xmanifest",
    show_manifest_address: false,
    show_original_filename: false,
    variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
  };
  let detailFails = true;

  axios.get.mockImplementation((url) => {
    if (url === "/api/auth/me") {
      return Promise.resolve({ data: { username: "admin" } });
    }
    if (url === "/api/admin/videos") {
      return Promise.resolve({ data: [adminVideo] });
    }
    if (url === "/api/videos") {
      return Promise.resolve({ data: [] });
    }
    if (url === "/api/admin/videos/admin-1") {
      if (detailFails) return Promise.reject(new Error("Detail timed out"));
      return Promise.resolve({ data: detail });
    }
    return Promise.reject(new Error(`Unexpected GET ${url}`));
  });
  axios.delete.mockRejectedValue(new Error("Delete refused"));
  axios.patch.mockRejectedValue(new Error("Visibility refused"));

  await renderApp();
  await click(findButton("Manage"));
  await waitFor(() => expect(text()).toContain("Admin stream"));

  await click(findButton("Admin stream"));
  expect(text()).toContain("Detail timed out");

  detailFails = false;
  await click(findButton("Admin stream"));
  await waitFor(() => expect(text()).toContain("0xmanifest"));

  await click(findButton("Delete"));
  expect(text()).toContain("Delete refused");
  expect(text()).toContain("Admin stream");

  const publishFilename = Array.from(container.querySelectorAll(".visibility-panel input"))
    .find((input) => input.type === "checkbox");
  await act(async () => {
    publishFilename.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flushPromises();

  expect(text()).toContain("Visibility refused");
  expect(text()).toContain("Admin stream");
});
