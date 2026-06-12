import type { ResolutionOption } from "./types";

export const BRAND_IMAGE = "/autvid-brand.png";
export const PLAYER_CONTROLS_IDLE_MS = 2200;
export const RESUME_DURATION_TOLERANCE_SECONDS = 0.25;

export const RESOLUTION_OPTIONS: ResolutionOption[] = [
  { value: "8k", label: "8K", width: 7680, height: 4320, bitrate: "~80 Mbps", defaultVideoBitrateKbps: 80_000, note: "maximum archive" },
  { value: "4k", label: "4K", width: 3840, height: 2160, bitrate: "~45 Mbps", defaultVideoBitrateKbps: 45_000, note: "ultra HD" },
  { value: "1440p", label: "1440P", width: 2560, height: 1440, bitrate: "~24 Mbps", defaultVideoBitrateKbps: 24_000, note: "quad HD" },
  { value: "1080p", label: "1080P", width: 1920, height: 1080, bitrate: "~12 Mbps", defaultVideoBitrateKbps: 12_000, note: "full HD" },
  { value: "720p", label: "720p", width: 1280, height: 720, bitrate: "~7.5 Mbps", defaultVideoBitrateKbps: 7_500, note: "HD" },
  { value: "540p", label: "540p", width: 960, height: 540, bitrate: "~4.5 Mbps", defaultVideoBitrateKbps: 4_500, note: "qHD" },
  { value: "480p", label: "480P", width: 854, height: 480, bitrate: "~3 Mbps", defaultVideoBitrateKbps: 3_000, note: "mobile" },
  { value: "360p", label: "360P", width: 640, height: 360, bitrate: "~1.5 Mbps", defaultVideoBitrateKbps: 1_500, note: "low bandwidth" },
  { value: "240p", label: "240p", width: 426, height: 240, bitrate: "~800 kbps", defaultVideoBitrateKbps: 800, note: "very low bandwidth" },
  { value: "144p", label: "144p", width: 256, height: 144, bitrate: "~350 kbps", defaultVideoBitrateKbps: 350, note: "minimum preview" },
];
