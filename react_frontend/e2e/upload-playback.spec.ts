import { expect, test, type Page } from "@playwright/test";
import path from "node:path";

const adminUsername = process.env.E2E_ADMIN_USERNAME || "admin";
const adminPassword = process.env.E2E_ADMIN_PASSWORD || "admin";
const fixturePath = process.env.E2E_VIDEO_PATH
  || path.resolve(__dirname, "../../testvids/sample.mp4");

async function csrfHeader(page: Page) {
  const csrf = (await page.context().cookies())
    .find((cookie) => cookie.name === "autvid_csrf")
    ?.value;
  expect(csrf).toBeTruthy();
  return { "X-CSRF-Token": csrf! };
}

test("login, upload, approve, publish, and play an HLS segment", async ({ page }) => {
  await page.goto("/login");
  await page.getByLabel("Username").fill(adminUsername);
  await page.getByLabel("Password").fill(adminPassword);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("button", { name: "Upload" })).toBeVisible();

  await page.getByRole("button", { name: "Upload" }).click();
  await page.locator('input[type="file"]').setInputFiles(fixturePath);
  await expect(page.getByText(/selected/i)).toBeVisible();
  await page.getByRole("button", { name: /^Upload / }).click();

  await expect(page).toHaveURL(/\/manage/);
  await page.waitForResponse((response) => (
    response.url().includes("/api/admin/videos") && response.status() === 200
  ));

  const deadline = Date.now() + 180_000;
  let videoId = "";
  let manifestAddress = "";
  while (Date.now() < deadline) {
    const videos = await page.request.get("/api/admin/videos");
    expect(videos.ok()).toBeTruthy();
    const [latest] = await videos.json();
    videoId = latest?.id || "";
    if (latest?.status === "awaiting_approval") {
      await page.request.post(`/api/admin/videos/${videoId}/approve`, {
        headers: await csrfHeader(page),
      });
    }
    if (latest?.status === "ready" || latest?.status === "published") {
      const detail = await page.request.get(`/api/admin/videos/${videoId}`);
      const body = await detail.json();
      manifestAddress = body.manifest_address || "";
      if (!body.is_public) {
        await page.request.patch(`/api/admin/videos/${videoId}/publication`, {
          data: { is_public: true },
          headers: await csrfHeader(page),
        });
      }
      break;
    }
    await page.waitForTimeout(5_000);
  }

  expect(videoId).toBeTruthy();
  await page.goto(`/library/${videoId}`);
  await expect(page.locator("video")).toBeVisible();

  const playlistUrl = manifestAddress
    ? `/stream/manifest/${manifestAddress}/360p/playlist.m3u8`
    : `/stream/${videoId}/360p/playlist.m3u8`;
  const playlist = await page.request.get(playlistUrl);
  expect(playlist.ok()).toBeTruthy();
  const manifest = await playlist.text();
  const firstSegment = manifest.split("\n").find((line) => line && !line.startsWith("#"));
  expect(firstSegment).toBeTruthy();
  const segmentUrl = firstSegment!.startsWith("/")
    ? firstSegment!
    : playlistUrl.replace(/\/[^/]+$/, `/${firstSegment}`);
  const segment = await page.request.get(segmentUrl);
  expect(segment.ok()).toBeTruthy();
  expect((await segment.body()).byteLength).toBeGreaterThan(0);
});
