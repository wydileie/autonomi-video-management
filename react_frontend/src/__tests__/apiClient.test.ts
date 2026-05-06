import { afterEach, expect, test, vi } from "vitest";

import { AUTH_STORAGE_KEY } from "../constants";
import type { AuthState } from "../types";

type RejectedInterceptor = (error: unknown) => Promise<unknown>;

type MockAxios = {
  delete: ReturnType<typeof vi.fn>;
  get: ReturnType<typeof vi.fn>;
  interceptors: {
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
  let rejectedInterceptor: RejectedInterceptor | null = null;
  const axiosMock: MockAxios = {
    delete: vi.fn(),
    get: vi.fn(),
    interceptors: {
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

  vi.doMock("axios", () => ({ default: axiosMock }));
  const client = await import("../api/client");
  if (!rejectedInterceptor) throw new Error("API client did not install a response interceptor");
  return { axiosMock, client, rejectedInterceptor };
}

afterEach(() => {
  vi.useRealTimers();
  vi.resetModules();
  vi.doUnmock("axios");
  window.localStorage.clear();
});

test("shares one refresh across concurrent 401s and replaces stale retry headers", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const refresh = deferred<{ data: AuthState }>();
  window.localStorage.setItem(AUTH_STORAGE_KEY, "old-token");
  axiosMock.post.mockReturnValue(refresh.promise);
  axiosMock.request.mockImplementation((config) => Promise.resolve({ data: { url: config.url } }));

  const first = rejectedInterceptor({
    config: {
      headers: { Authorization: "Bearer old-token" },
      method: "get",
      url: "/api/admin/videos",
    },
    response: { status: 401 },
  });
  const second = rejectedInterceptor({
    config: {
      headers: { Authorization: "Bearer old-token" },
      method: "get",
      url: "/api/admin/videos/vid-1",
    },
    response: { status: 401 },
  });

  expect(axiosMock.post).toHaveBeenCalledTimes(1);
  expect(axiosMock.post).toHaveBeenCalledWith("/api/auth/refresh", null, { withCredentials: true });

  refresh.resolve({ data: { access_token: "fresh-token", username: "admin" } });

  await expect(first).resolves.toEqual({ data: { url: "/api/admin/videos" } });
  await expect(second).resolves.toEqual({ data: { url: "/api/admin/videos/vid-1" } });
  expect(window.localStorage.getItem(AUTH_STORAGE_KEY)).toBe("fresh-token");
  expect(axiosMock.request).toHaveBeenCalledTimes(2);
  expect(axiosMock.request.mock.calls[0][0].headers.Authorization).toBe("Bearer fresh-token");
  expect(axiosMock.request.mock.calls[1][0].headers.Authorization).toBe("Bearer fresh-token");
});

test("clears stored auth and notifies listeners when refresh fails", async () => {
  const { axiosMock, client, rejectedInterceptor } = await loadClient();
  const refreshError = new Error("Refresh expired");
  const refreshEvents: Array<AuthState | null> = [];
  window.localStorage.setItem(AUTH_STORAGE_KEY, "old-token");
  client.subscribeAuthRefresh((auth) => refreshEvents.push(auth));
  axiosMock.post.mockRejectedValue(refreshError);

  await expect(rejectedInterceptor({
    config: {
      headers: { Authorization: "Bearer old-token" },
      method: "get",
      url: "/api/admin/videos",
    },
    response: { status: 401 },
  })).rejects.toBe(refreshError);

  expect(window.localStorage.getItem(AUTH_STORAGE_KEY)).toBeNull();
  expect(refreshEvents).toEqual([null]);
  expect(axiosMock.request).not.toHaveBeenCalled();
});

test("does not refresh recursively for auth endpoint 401s", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const authError = {
    config: { method: "post", url: "/api/auth/refresh" },
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
    config: { method: "get", url: "/api/videos" },
    response: { status: 503 },
  });
  await vi.advanceTimersByTimeAsync(150);
  await expect(readRetry).resolves.toEqual({ data: "ok" });

  const quoteRetry = rejectedInterceptor({
    config: { method: "post", url: "/api/videos/upload/quote" },
    response: { status: 502 },
  });
  await vi.advanceTimersByTimeAsync(150);
  await expect(quoteRetry).resolves.toEqual({ data: "ok" });

  expect(axiosMock.request).toHaveBeenCalledTimes(2);
  expect(axiosMock.request.mock.calls[0][0]).toMatchObject({
    _transientRetryCount: 1,
    method: "get",
    url: "/api/videos",
  });
  expect(axiosMock.request.mock.calls[1][0]).toMatchObject({
    _transientRetryCount: 1,
    method: "post",
    url: "/api/videos/upload/quote",
  });
});

test("does not retry file upload, approve, or delete requests after 5xx errors", async () => {
  const { axiosMock, rejectedInterceptor } = await loadClient();
  const uploadError = {
    config: { method: "post", url: "/api/videos/upload" },
    response: { status: 503 },
  };
  const approveError = {
    config: { method: "post", url: "/api/admin/videos/vid-1/approve" },
    response: { status: 503 },
  };
  const deleteError = {
    config: { method: "delete", url: "/api/admin/videos/vid-1" },
    response: { status: 503 },
  };

  await expect(rejectedInterceptor(uploadError)).rejects.toBe(uploadError);
  await expect(rejectedInterceptor(approveError)).rejects.toBe(approveError);
  await expect(rejectedInterceptor(deleteError)).rejects.toBe(deleteError);

  expect(axiosMock.post).not.toHaveBeenCalled();
  expect(axiosMock.request).not.toHaveBeenCalled();
});
