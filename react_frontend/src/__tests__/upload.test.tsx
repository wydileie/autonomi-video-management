import { act } from "react";
import {
  AUTH_STORAGE_KEY,
  axios,
  click,
  container,
  findButton,
  findCheckbox,
  flushPromises,
  renderApp,
  setupGetRoutes,
  text,
} from "./testUtils";

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
        resolutions: ["1080p", "720p", "540p", "480p", "360p", "240p", "144p"],
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

test("sends original-source and auto-publish options with an upload", async () => {
  vi.useFakeTimers();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  setupGetRoutes();

  const realCreateElement = document.createElement.bind(document);
  vi.spyOn(document, "createElement").mockImplementation((tagName, options) => {
    const element = realCreateElement(tagName, options);
    if (tagName === "video") {
      Object.defineProperties(element, {
        duration: { configurable: true, value: 12 },
        videoHeight: { configurable: true, value: 720 },
        videoWidth: { configurable: true, value: 1280 },
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

  let uploadedForm;
  axios.post.mockImplementation((url, body) => {
    if (url === "/api/videos/upload/quote") {
      expect(body).toMatchObject({
        duration_seconds: 12,
        upload_original: true,
        source_size_bytes: 15,
      });
      return Promise.resolve({
        data: {
          estimated_bytes: 9000,
          estimated_gas_cost_wei: "77",
          original_file: { estimated_bytes: 15 },
          payment_mode: "single",
          sampled: false,
          segment_count: 2,
          storage_cost_atto: "1000000000000000000",
        },
      });
    }
    if (url === "/api/videos/upload") {
      uploadedForm = body;
      return Promise.resolve({
        data: {
          created_at: "2026-04-27T12:00:00Z",
          id: "uploaded",
          status: "pending",
          title: "source",
        },
      });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Upload"));

  const fileInput = container.querySelector('input[type="file"]');
  const file = new File(["original source"], "source.mp4", { type: "video/mp4" });
  expect(file.size).toBe(15);
  await act(async () => {
    Object.defineProperty(fileInput, "files", { configurable: true, value: [file] });
    fileInput.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await click(findCheckbox("Upload original source file"));
  await click(findCheckbox("Publish automatically when ready"));

  await act(async () => {
    vi.advanceTimersByTime(260);
  });
  await flushPromises();

  expect(text()).toContain("original file");
  await click(findButton("Upload source"));

  expect(uploadedForm.get("upload_original")).toBe("true");
  expect(uploadedForm.get("publish_when_ready")).toBe("true");
  expect(uploadedForm.get("show_manifest_address")).toBe("false");
  expect(uploadedForm.get("file")).toBe(file);
});

test("shows quote and upload errors without clearing the selected source", async () => {
  vi.useFakeTimers();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  setupGetRoutes();

  const realCreateElement = document.createElement.bind(document);
  vi.spyOn(document, "createElement").mockImplementation((tagName, options) => {
    const element = realCreateElement(tagName, options);
    if (tagName === "video") {
      Object.defineProperties(element, {
        duration: { configurable: true, value: 20 },
        videoHeight: { configurable: true, value: 720 },
        videoWidth: { configurable: true, value: 1280 },
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

  let quoteAttempts = 0;
  axios.post.mockImplementation((url, body, config) => {
    if (url === "/api/videos/upload/quote") {
      quoteAttempts += 1;
      if (quoteAttempts === 1) {
        return Promise.reject({ response: { data: { detail: "Quote service unavailable" } } });
      }
      return Promise.resolve({
        data: {
          estimated_bytes: 2048,
          estimated_gas_cost_wei: "42",
          payment_mode: "single",
          sampled: false,
          segment_count: 1,
          storage_cost_atto: "1000000000000000000",
        },
      });
    }
    if (url === "/api/videos/upload") {
      config.onUploadProgress({ loaded: 5, total: 10 });
      return Promise.reject({ response: { data: { detail: "Upload disk full" } } });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Upload"));

  const fileInput = container.querySelector('input[type="file"]');
  const file = new File(["source bytes"], "failure.mp4", { type: "video/mp4" });
  await act(async () => {
    Object.defineProperty(fileInput, "files", { configurable: true, value: [file] });
    fileInput.dispatchEvent(new Event("change", { bubbles: true }));
  });

  await act(async () => {
    vi.advanceTimersByTime(260);
  });
  await flushPromises();
  expect(text()).toContain("Quote service unavailable");

  await click(findButton("Current only"));
  await act(async () => {
    vi.advanceTimersByTime(260);
  });
  await flushPromises();
  expect(text()).toContain("1 ANT");

  await click(findButton("Upload source"));
  await flushPromises();
  expect(text()).toContain("Upload disk full");
  expect(text()).toContain("failure.mp4");
});
