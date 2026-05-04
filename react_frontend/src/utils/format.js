/* global BigInt */
export function formatBytes(bytes) {
  if (!Number.isFinite(bytes)) return "";
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(size >= 10 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}

export function formatAttoTokens(value) {
  try {
    const atto = BigInt(value || "0");
    const sign = atto < 0n ? "-" : "";
    const magnitude = atto < 0n ? -atto : atto;
    const scale = 1000000000000000000n;
    const whole = magnitude / scale;
    const fraction = magnitude % scale;
    if (fraction === 0n) return `${sign}${whole.toString()} ANT`;

    const trimmed = fraction.toString().padStart(18, "0").replace(/0+$/, "");
    const display = trimmed.slice(0, 6).padEnd(Math.min(trimmed.length, 6), "0");
    if (whole === 0n && trimmed.length > 6 && /^0*$/.test(display)) {
      return sign ? ">-0.000001 ANT" : "<0.000001 ANT";
    }
    return `${sign}${whole.toString()}.${display} ANT`;
  } catch {
    return `${value || "0"} atto`;
  }
}

export function formatWei(value) {
  try {
    return `${BigInt(value || "0").toLocaleString()} wei`;
  } catch {
    return `${value || "0"} wei`;
  }
}

export function formatDateTime(value) {
  if (!value) return "";
  return new Date(value).toLocaleString();
}
