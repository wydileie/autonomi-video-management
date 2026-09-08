import { act } from "react";
import { vi } from "vitest";
import {
  axios,
  click,
  findButton,
  flushPromises,
  renderApp,
  setAuthenticatedCookies,
  setupGetRoutes,
  text,
} from "./testUtils";

test("approves exactly the displayed catalog caps and resumes the same paused publication", async () => {
  setAuthenticatedCookies();
  setupGetRoutes();
  const approval = {
    quote_id: "catalog-quote",
    content_digest: "catalog-digest",
    network: "test:1",
    expires_at: 2000000000,
    max_storage_atto: "1500000000000000000",
    max_gas_wei: "7000",
  };
  let state = "draft";
  let approvalFails = true;
  const get = axios.get.getMockImplementation();
  axios.get.mockImplementation((url, config) =>
    url === "/admin/catalogs"
      ? Promise.resolve({
          data: {
            published_catalog_address: null,
            all_catalog_address: null,
            publication: { state, approval },
          },
        })
      : get(url, config),
  );
  const post = axios.post.getMockImplementation();
  axios.post.mockImplementation((url, body) => {
    if (url === "/admin/catalogs/approve") {
      expect(body).toEqual({
        quote_id: approval.quote_id,
        max_storage_atto: approval.max_storage_atto,
        max_gas_wei: approval.max_gas_wei,
      });
      if (approvalFails)
        return Promise.reject({ response: { data: { detail: "Review the current caps" } } });
      state = "payment_recovery_required";
      return Promise.resolve({ data: {} });
    }
    if (url === "/admin/catalogs/resume") {
      expect(body).toBeNull();
      state = "uploading";
      return Promise.resolve({ data: {} });
    }
    return post(url, body);
  });

  await renderApp();
  await click(findButton("Manage"));
  expect(text()).toContain("Storage cap:");
  expect(text()).toContain("Gas cap:");
  await click(findButton("Approve catalog storage and gas caps"));
  expect(text()).toContain("Review the current caps");
  expect(findButton("Approve catalog storage and gas caps").disabled).toBe(false);

  approvalFails = false;
  await click(findButton("Approve catalog storage and gas caps"));
  expect(findButton("Quote catalog publication").disabled).toBe(true);
  await click(findButton("Resume approved catalog publication"));
  expect(text()).toContain("Publishing the approved catalog snapshot");
  expect(axios.post.mock.calls.filter(([url]) => url === "/admin/catalogs/publish")).toHaveLength(
    0,
  );
});

test("catalog polling waits for the current response and discards it on leaving admin", async () => {
  setAuthenticatedCookies();
  setupGetRoutes();
  let calls = 0;
  let signal: AbortSignal | undefined;
  let resolveCatalog: (value: unknown) => void;
  const get = axios.get.getMockImplementation();
  axios.get.mockImplementation((url, config) => {
    if (url !== "/admin/catalogs") return get(url, config);
    calls += 1;
    signal = config.signal;
    return new Promise((resolve) => {
      resolveCatalog = resolve;
    });
  });
  await renderApp();
  vi.useFakeTimers();
  await click(findButton("Manage"));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(15000);
  });
  expect(calls).toBe(1);
  expect(signal?.aborted).toBe(false);
  await click(findButton(/^Library$/));
  expect(signal?.aborted).toBe(true);
  await act(async () => {
    resolveCatalog({ data: { published_catalog_address: "stale-private-catalog" } });
  });
  await flushPromises();
  expect(text()).not.toContain("stale-private-catalog");
  expect(text()).not.toContain("Portable catalogs");
});
