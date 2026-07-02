import {
  axios,
  click,
  container,
  findButton,
  renderApp,
  setAuthenticatedCookies,
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

test("logs in with cookie-only auth and sends no bearer headers on admin requests", async () => {
  setupGetRoutes();
  axios.post.mockImplementation((url, body) => {
    if (url === "/auth/login") {
      expect(body).toEqual({ username: "admin", password: "secret" });
      setAuthenticatedCookies();
      return Promise.resolve({ data: { username: "admin" } });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Login"));
  setInputValue(container.querySelector('input[type="password"]'), "secret");
  await click(findButton("Sign in"));

  await waitFor(() => {
    expect(window.localStorage.length).toBe(0);
    expect(text()).toContain("No videos yet. Upload one to build your first stream.");
  });
  expect(text()).toContain("Manage");
  expect(text()).toContain("Upload");
  expect(text()).toContain("Logout");
  expect(axios.get).toHaveBeenCalledWith("/auth/me");
  expect(axios.get).toHaveBeenCalledWith("/admin/videos");
});

test("restores an admin session from the refresh cookie", async () => {
  setAuthenticatedCookies();
  setupGetRoutes();
  axios.post.mockImplementation((url, body, config) => {
    if (url === "/auth/refresh") {
      expect(body).toBeNull();
      expect(config).toBeUndefined();
      return Promise.resolve({
        data: {
          refresh_token_expires_at: "2026-05-27T12:00:00Z",
          username: "admin",
        },
      });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();

  await waitFor(() => {
    expect(window.localStorage.length).toBe(0);
    expect(text()).toContain("Manage");
    expect(text()).toContain("Upload");
  });
  expect(axios.get).toHaveBeenCalledWith("/auth/me");
});

test("logs out through the backend and clears local admin auth", async () => {
  setAuthenticatedCookies();
  setupGetRoutes();
  axios.post.mockImplementation((url, body) => {
    if (url === "/auth/refresh") {
      return Promise.resolve({ data: { username: "admin" } });
    }
    if (url === "/auth/logout") {
      expect(body).toBeNull();
      return Promise.resolve({ data: { ok: true } });
    }
    return Promise.reject(new Error(`Unexpected POST ${url}`));
  });

  await renderApp();
  await click(findButton("Logout"));

  await waitFor(() => {
    expect(axios.post).toHaveBeenCalledWith("/auth/logout", null);
    expect(window.localStorage.length).toBe(0);
    expect(text()).toContain("Login");
  });
  expect(text()).not.toContain("Logout");
});
