import {
  AUTH_STORAGE_KEY,
  axios,
  click,
  container,
  findButton,
  renderApp,
  setupGetRoutes,
  setInputValue,
  text,
  waitFor,
} from "./testUtils";
import { formatAttoTokens } from "../App";

test("formats signed atto token values without a malformed fractional sign", () => {
  expect(formatAttoTokens("10000000000000000000")).toBe("10 ANT");
  expect(formatAttoTokens("-5322870000000000000")).toBe("-5.32287 ANT");
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

test("restores an admin session from the refresh cookie", async () => {
  setupGetRoutes();
  axios.post.mockImplementation((url, body, config) => {
    if (url === "/api/auth/refresh") {
      expect(body).toBeNull();
      expect(config).toEqual({ withCredentials: true });
      return Promise.resolve({
        data: {
          access_token: "fresh-token",
          refresh_token_expires_at: "2026-05-27T12:00:00Z",
          token_type: "bearer",
          username: "admin",
        },
      });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();

  await waitFor(() => {
    expect(window.localStorage.getItem(AUTH_STORAGE_KEY)).toBe("fresh-token");
    expect(text()).toContain("Manage");
    expect(text()).toContain("Upload");
  });
  expect(axios.get).toHaveBeenCalledWith(
    "/api/auth/me",
    { headers: { Authorization: "Bearer fresh-token" } },
  );
});

test("logs out through the backend and clears local admin auth", async () => {
  window.localStorage.setItem(AUTH_STORAGE_KEY, "stored-token");
  setupGetRoutes();
  axios.post.mockImplementation((url) => {
    if (url === "/api/auth/logout") {
      return Promise.resolve({ data: { ok: true } });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Logout"));

  await waitFor(() => {
    expect(axios.post).toHaveBeenCalledWith("/api/auth/logout", null, { withCredentials: true });
    expect(window.localStorage.getItem(AUTH_STORAGE_KEY)).toBeNull();
    expect(text()).toContain("Login");
  });
  expect(text()).not.toContain("Logout");
});
