import { act } from "react";
import {
  axios,
  click,
  container,
  findButton,
  Hls,
  mouseLeave,
  renderApp,
  setupGetRoutes,
  text,
  waitFor,
} from "./testUtils";

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
    recoverMediaError: vi.fn(),
    startLoad: vi.fn(),
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

test("configures hls.js retries and recovers fatal network and media errors", async () => {
  Hls.isSupported.mockReturnValue(true);
  const hlsInstance = {
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
    recoverMediaError: vi.fn(),
    startLoad: vi.fn(),
  };
  Hls.mockImplementation(() => hlsInstance);
  const publicVideo = {
    created_at: "2026-04-27T12:00:00Z",
    description: "Public description only",
    id: "pub-retry",
    original_filename: "",
    status: "ready",
    title: "Retry stream",
  };
  setupGetRoutes({
    publicVideos: [publicVideo],
    details: {
      "/api/videos/pub-retry": {
        ...publicVideo,
        manifest_address: null,
        variants: [{ id: "variant-1", resolution: "720p", segment_count: 4 }],
      },
    },
  });

  await renderApp();
  await waitFor(() => expect(text()).toContain("Retry stream"));
  await click(findButton("Retry stream"));

  expect(Hls).toHaveBeenCalledWith(expect.objectContaining({
    fragLoadingMaxRetry: 4,
    levelLoadingMaxRetry: 4,
    manifestLoadingMaxRetry: 4,
  }));

  const errorHandler = hlsInstance.on.mock.calls
    .find(([eventName]) => eventName === Hls.Events.ERROR)[1];
  await act(async () => {
    errorHandler(Hls.Events.ERROR, { fatal: true, type: Hls.ErrorTypes.NETWORK_ERROR });
  });
  await act(async () => {
    errorHandler(Hls.Events.ERROR, { fatal: true, type: Hls.ErrorTypes.MEDIA_ERROR });
  });

  expect(hlsInstance.startLoad).toHaveBeenCalled();
  expect(hlsInstance.recoverMediaError).toHaveBeenCalled();
  expect(text()).not.toContain("Playback failed because the video segments could not be loaded.");
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

test("shows a catalog load error when the initial library request fails", async () => {
  axios.get.mockRejectedValue(new Error("Catalog offline"));

  await renderApp();

  await waitFor(() => expect(text()).toContain("Catalog offline"));
  expect(text()).not.toContain("No videos are available yet.");
});
