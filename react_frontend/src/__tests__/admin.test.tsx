import { act } from "react";
import { vi } from "vitest";
import {
  AUTH_STORAGE_KEY,
  axios,
  click,
  container,
  findButton,
  flushPromises,
  Hls,
  renderApp,
  setupGetRoutes,
  text,
  waitFor,
} from "./testUtils";

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

test("polls the admin library while videos are actively processing", async () => {
  vi.useFakeTimers();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  let adminListCalls = 0;
  axios.get.mockImplementation((url) => {
    if (url === "/api/auth/me") {
      return Promise.resolve({ data: { username: "admin" } });
    }
    if (url === "/api/videos") {
      return Promise.resolve({ data: [] });
    }
    if (url === "/api/admin/videos") {
      adminListCalls += 1;
      return Promise.resolve({
        data: [{
          created_at: "2026-04-27T12:00:00Z",
          id: "vid-processing",
          status: "processing",
          title: `Processing ${adminListCalls}`,
        }],
      });
    }
    return Promise.reject(new Error(`Unexpected GET ${url}`));
  });

  await renderApp();
  await click(findButton("Manage"));
  expect(text()).toContain("Processing 1");

  await act(async () => {
    vi.advanceTimersByTime(5000);
  });
  await flushPromises();

  expect(adminListCalls).toBeGreaterThanOrEqual(2);
  expect(text()).toContain("Processing 2");
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
