import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { vi } from "vitest";
import App from "../App";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type TestHlsMock = ReturnType<typeof vi.fn> & {
  Events: { ERROR: string; MANIFEST_PARSED: string };
  isSupported: ReturnType<typeof vi.fn>;
};

type RouteDetails = Record<string, unknown>;

type SetupGetRoutesOptions = {
  publicVideos?: unknown[];
  adminVideos?: unknown[];
  details?: RouteDetails;
  currentUser?: unknown;
};

const mockAxios = vi.hoisted(() => ({
  get: vi.fn(),
  post: vi.fn(),
  patch: vi.fn(),
  delete: vi.fn(),
  isCancel: vi.fn(() => false),
}));

const mockHls = vi.hoisted(() => {
  const HlsMock = vi.fn().mockImplementation(() => ({
    attachMedia: vi.fn(),
    destroy: vi.fn(),
    loadSource: vi.fn(),
    on: vi.fn(),
  })) as TestHlsMock;
  HlsMock.Events = { ERROR: "hlsError", MANIFEST_PARSED: "manifestParsed" };
  HlsMock.isSupported = vi.fn(() => false);
  return HlsMock;
});

vi.mock("axios", () => ({
  default: mockAxios,
}));

vi.mock("hls.js", () => {
  return { default: mockHls };
});

export const AUTH_STORAGE_KEY = "autvid_admin_token";
export { mockAxios as axios, mockHls as Hls };

export let container: HTMLDivElement;
let root: Root | null = null;

export const flushPromises = () => act(async () => {
  await Promise.resolve();
  await Promise.resolve();
});

export function text(): string {
  return container.textContent;
}

export function findButton(label: string | RegExp): HTMLButtonElement {
  const matcher = label instanceof RegExp ? label : new RegExp(label, "i");
  const button = Array.from(container.querySelectorAll("button"))
    .find((candidate) => matcher.test(candidate.textContent ?? ""));
  if (!button) throw new Error(`Unable to find button matching ${matcher}`);
  return button;
}

export function findCheckbox(label: string | RegExp): HTMLInputElement {
  const matcher = label instanceof RegExp ? label : new RegExp(label, "i");
  const input = Array.from(container.querySelectorAll("label"))
    .find((candidate) => matcher.test(candidate.textContent ?? ""))
    ?.querySelector<HTMLInputElement>('input[type="checkbox"]');
  if (!input) throw new Error(`Unable to find checkbox matching ${matcher}`);
  return input;
}

export function setInputValue(input: HTMLInputElement | null, value: string) {
  if (!input) throw new Error("Input not found");
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  if (!setter) throw new Error("HTMLInputElement value setter not found");
  act(() => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

export async function click(element: Element) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flushPromises();
}

export async function mouseLeave(element: Element) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("mouseout", {
      bubbles: true,
      relatedTarget: document.body,
    }));
  });
  await flushPromises();
}

export async function renderApp() {
  container = document.createElement("div");
  document.body.appendChild(container);
  const appRoot = createRoot(container);
  root = appRoot;
  await act(async () => {
    appRoot.render(<App />);
  });
  await flushPromises();
}

export async function waitFor<T>(assertion: () => T): Promise<T> {
  const started = Date.now();
  let lastError: unknown;
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

export function setupGetRoutes({
  publicVideos = [],
  adminVideos = [],
  details = {},
  currentUser = { username: "admin" },
}: SetupGetRoutesOptions = {}) {
  mockAxios.get.mockImplementation((url: string, config = {}) => {
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
  window.history.replaceState({}, "", "/");
  mockHls.isSupported.mockReturnValue(false);
  mockHls.mockImplementation(() => ({
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
  const currentRoot = root;
  if (currentRoot) {
    await act(async () => {
      currentRoot.unmount();
    });
  }
  if (container) {
    container.remove();
  }
  vi.useRealTimers();
  root = null;
  container = undefined as unknown as HTMLDivElement;
  vi.restoreAllMocks();
});
