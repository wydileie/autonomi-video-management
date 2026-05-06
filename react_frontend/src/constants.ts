import type { ResolutionOption } from "./types";

export const BRAND_IMAGE = "/autvid-brand.png";
export const AUTH_STORAGE_KEY = "autvid_admin_token";
export const PLAYER_CONTROLS_IDLE_MS = 2200;
export const RESUME_DURATION_TOLERANCE_SECONDS = 0.25;

export const RESOLUTION_OPTIONS: ResolutionOption[] = [
  { value: "8k", label: "8K", width: 7680, height: 4320, bitrate: "~45 Mbps", note: "maximum archive" },
  { value: "4k", label: "4K", width: 3840, height: 2160, bitrate: "~16 Mbps", note: "ultra HD" },
  { value: "1440p", label: "1440P", width: 2560, height: 1440, bitrate: "~8 Mbps", note: "quad HD" },
  { value: "1080p", label: "1080P", width: 1920, height: 1080, bitrate: "~5 Mbps", note: "full HD" },
  { value: "720p", label: "720p", width: 1280, height: 720, bitrate: "~2.5 Mbps", note: "HD" },
  { value: "540p", label: "540p", width: 960, height: 540, bitrate: "~1.6 Mbps", note: "qHD" },
  { value: "480p", label: "480P", width: 854, height: 480, bitrate: "~1 Mbps", note: "mobile" },
  { value: "360p", label: "360P", width: 640, height: 360, bitrate: "~500 kbps", note: "low bandwidth" },
  { value: "240p", label: "240p", width: 426, height: 240, bitrate: "~300 kbps", note: "very low bandwidth" },
  { value: "144p", label: "144p", width: 256, height: 144, bitrate: "~150 kbps", note: "minimum preview" },
];
