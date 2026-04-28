import React, { act } from "react";
import { createRoot } from "react-dom/client";
import axios from "axios";
import App from "./App";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

jest.mock("axios", () => ({
  get: jest.fn(),
  post: jest.fn(),
  patch: jest.fn(),
  delete: jest.fn(),
  isCancel: jest.fn(() => false),
}));

jest.mock("hls.js", () => {
  const Hls = jest.fn().mockImplementation(() => ({
    attachMedia: jest.fn(),
    destroy: jest.fn(),
    loadSource: jest.fn(),
    on: jest.fn(),
  }));
  Hls.Events = { MANIFEST_PARSED: "manifestParsed" };
  Hls.isSupported = jest.fn(() => false);
  return Hls;
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
  jest.useRealTimers();
  jest.clearAllMocks();
  window.localStorage.clear();
  if (!URL.createObjectURL) {
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: jest.fn(),
    });
  }
  if (!URL.revokeObjectURL) {
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: jest.fn(),
    });
  }
  jest.spyOn(window, "confirm").mockReturnValue(true);
  jest.spyOn(window, "alert").mockImplementation(() => {});
  jest.spyOn(URL, "createObjectURL").mockReturnValue("blob:video");
  jest.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
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
  jest.useRealTimers();
  root = null;
  container = null;
  jest.restoreAllMocks();
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
  jest.useFakeTimers();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  setupGetRoutes();

  const realCreateElement = document.createElement.bind(document);
  jest.spyOn(document, "createElement").mockImplementation((tagName, options) => {
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
    jest.advanceTimersByTime(260);
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
  expect(text()).not.toContain("Manifest hidden or pending");
  expect(text()).not.toContain("Delete");
  expect(text()).not.toContain("Publish manifest address");
});
