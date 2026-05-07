import { afterEach, expect, test, vi } from "vitest";

import type { AuthState } from "../types";

type FulfilledInterceptor = (config: Record<string, unknown>) => Record<string, unknown>;
type RejectedInterceptor = (error: unknown) => Promise<unknown>;

type MockAxios = {
  create: ReturnType<typeof vi.fn>;
  delete: ReturnType<typeof vi.fn>;
  get: ReturnType<typeof vi.fn>;
  interceptors: {
    request: {
      use: ReturnType<typeof vi.fn>;
    };
    response: {
      use: ReturnType<typeof vi.fn>;
    };
  };
  isCancel: ReturnType<typeof vi.fn>;
  patch: ReturnType<typeof vi.fn>;
  post: ReturnType<typeof vi.fn>;
  request: ReturnType<typeof vi.fn>;
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, reject, resolve };
}

async function loadClient() {
  vi.resetModules();
  let requestInterceptor: FulfilledInterceptor | null = null;
  let rejectedInterceptor: RejectedInterceptor | null = null;
  const axiosMock: MockAxios = {
    create: vi.fn(),
    delete: vi.fn(),
    get: vi.fn(),
    interceptors: {
      request: {
        use: vi.fn((onFulfilled) => {
          requestInterceptor = onFulfilled;
        }),
      },
      response: {
        use: vi.fn((_onFulfilled, onRejected) => {
          rejectedInterceptor = onRejected;
        }),
      },
    },
    isCancel: vi.fn(() => false),
    patch: vi.fn(),
    post: vi.fn(),
    request: vi.fn(),
  };
  axiosMock.create.mockReturnValue(axiosMock);

  vi.doMock("axios", () => ({ default: axiosMock }));
  const client = await import("../api/client");
  if (!requestInterceptor) throw new Error("API client did not install a request interceptor");
  if (!rejectedInterceptor) throw new Error("API client did not install a response interceptor");
  return { axiosMock, client, rejectedInterceptor, requestInterceptor };
}

afterEach(() => {
  vi.useRealTimers();
  vi.resetModules();
  vi.doUnmock("axios");
  window.localStorage.clear();
  document.cookie = "autvid_csrf=; Max-Age=0; path=/";
});

test("creates a cookie-only axios client with timeout and credentials", async () => {
  const { axiosMock } = await loadClient();

  expect(axiosMock.create).toHaveBeenCalledWith({
    baseURL: "/api",
    timeout: 60_000,
    withCredentials: true,
  });
});

test("adds CSRF to unsafe non-login requests without adding Authorization", async () => {
  const { requestInterceptor } = await loadClient();
  document.cookie = "autvid_csrf=csrf-123; path=/";

  const config = requestInterceptor({
    headers: {},
    method: "post",
    url: "/videos/upload",
  });

  expect(config.headers).toMatchObject({ "X-CSRF-Token": "csrf-123" });
  expect(config.headers).not.toHaveProperty("Authorization");
  expect(requestInterceptor({ method: "post", url: "/auth/login" })).not.toHaveProperty(
    "headers",
  );
});

test("shares one refresh across concurrent 401s and retries without bearer headers", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const refresh = deferred<{ data: AuthState }>();
  axiosMock.post.mockReturnValue(refresh.promise);
  axiosMock.request.mockImplementation((config) => Promise.resolve({ data: { url: config.url } }));

  const first = rejectedInterceptor({
    config: {
      headers: {},
      method: "get",
      url: "/admin/videos",
    },
    response: { status: 401 },
  });
  const second = rejectedInterceptor({
    config: {
      headers: {},
      method: "get",
      url: "/admin/videos/vid-1",
    },
    response: { status: 401 },
  });

  expect(axiosMock.post).toHaveBeenCalledTimes(1);
  expect(axiosMock.post).toHaveBeenCalledWith("/auth/refresh", null);

  refresh.resolve({ data: { username: "admin" } });

  await expect(first).resolves.toEqual({ data: { url: "/admin/videos" } });
  await expect(second).resolves.toEqual({ data: { url: "/admin/videos/vid-1" } });
  expect(window.localStorage.length).toBe(0);
  expect(axiosMock.request).toHaveBeenCalledTimes(2);
  expect(axiosMock.request.mock.calls[0][0].headers).not.toHaveProperty("Authorization");
  expect(axiosMock.request.mock.calls[1][0].headers).not.toHaveProperty("Authorization");
});

test("notifies listeners when refresh fails", async () => {
  const { axiosMock, client, rejectedInterceptor } = await loadClient();
  const refreshError = new Error("Refresh expired");
  const refreshEvents: Array<AuthState | null> = [];
  client.subscribeAuthRefresh((auth) => refreshEvents.push(auth));
  axiosMock.post.mockRejectedValue(refreshError);

  await expect(rejectedInterceptor({
    config: {
      headers: {},
      method: "get",
      url: "/admin/videos",
    },
    response: { status: 401 },
  })).rejects.toBe(refreshError);

  expect(refreshEvents).toEqual([null]);
  expect(axiosMock.request).not.toHaveBeenCalled();
});

test("does not refresh recursively for auth endpoint 401s", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const authError = {
    config: { method: "post", url: "/auth/refresh" },
    response: { status: 401 },
  };

  await expect(rejectedInterceptor(authError)).rejects.toBe(authError);

  expect(axiosMock.post).not.toHaveBeenCalled();
  expect(axiosMock.request).not.toHaveBeenCalled();
});

test("retries idempotent reads and upload quote requests after transient errors", async () => {
  vi.useFakeTimers();
  const { axiosMock, rejectedInterceptor } = await loadClient();
  axiosMock.request.mockResolvedValue({ data: "ok" });

  const readRetry = rejectedInterceptor({
    config: { method: "get", url: "/videos" },
    response: { status: 503 },
  });
  await vi.advanceTimersByTimeAsync(150);
  await expect(readRetry).resolves.toEqual({ data: "ok" });

  const quoteRetry = rejectedInterceptor({
    config: { method: "post", url: "/videos/upload/quote" },
    response: { status: 502 },
  });
  await vi.advanceTimersByTimeAsync(150);
  await expect(quoteRetry).resolves.toEqual({ data: "ok" });

  expect(axiosMock.request).toHaveBeenCalledTimes(2);
  expect(axiosMock.request.mock.calls[0][0]).toMatchObject({
    _transientRetryCount: 1,
    method: "get",
    url: "/videos",
  });
  expect(axiosMock.request.mock.calls[1][0]).toMatchObject({
    _transientRetryCount: 1,
    method: "post",
    url: "/videos/upload/quote",
  });
});

test("does not retry file upload, approve, or delete requests after 5xx errors", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const uploadError = {
    config: { method: "post", url: "/videos/upload" },
    response: { status: 503 },
  };
  const approveError = {
    config: { method: "post", url: "/admin/videos/vid-1/approve" },
    response: { status: 503 },
  };
  const deleteError = {
    config: { method: "delete", url: "/admin/videos/vid-1" },
    response: { status: 503 },
  };

  await expect(rejectedInterceptor(uploadError)).rejects.toBe(uploadError);
  await expect(rejectedInterceptor(approveError)).rejects.toBe(approveError);
  await expect(rejectedInterceptor(deleteError)).rejects.toBe(deleteError);

  expect(axiosMock.post).not.toHaveBeenCalled();
  expect(axiosMock.request).not.toHaveBeenCalled();
});

test("surfaces request IDs from error responses", async () => {
  const { client } = await loadClient();

  expect(client.requestErrorMessage({
    response: {
      data: { detail: "Quote failed" },
      headers: { "x-request-id": "req-123" },
    },
  }, "Fallback")).toBe("Quote failed (request req-123)");

  expect(client.requestErrorMessage({
    response: {
      data: { detail: "Upload failed", request_id: "req-456" },
      headers: { "x-request-id": "req-ignored" },
    },
  }, "Fallback")).toBe("Upload failed (request req-456)");
});
