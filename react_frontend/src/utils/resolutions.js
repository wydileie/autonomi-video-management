import { RESOLUTION_OPTIONS } from "../constants";

export function orderedSelection(selected) {
  return RESOLUTION_OPTIONS
    .map((option) => option.value)
    .filter((value) => selected.includes(value));
}

export function classifyResolution(width, height) {
  if (!width || !height) return null;
  const shortEdge = Math.min(width, height);
  return RESOLUTION_OPTIONS.find((option) => (
    shortEdge >= Math.min(option.width, option.height) * 0.92
  )) || RESOLUTION_OPTIONS[RESOLUTION_OPTIONS.length - 1];
}

export function optionFitsSource(option, meta) {
  if (!meta?.width || !meta?.height) return true;
  const shortEdge = Math.min(meta.width, meta.height);
  return shortEdge >= Math.min(option.width, option.height) * 0.92;
}

function evenFloor(value) {
  const floored = Math.floor(value);
  return Math.max(2, floored - (floored % 2));
}

function fitWithinSource(width, height, sourceWidth, sourceHeight) {
  if (width <= sourceWidth && height <= sourceHeight) return { width, height };
  const scale = Math.min(sourceWidth / width, sourceHeight / height, 1);
  return {
    width: evenFloor(width * scale),
    height: evenFloor(height * scale),
  };
}

export function targetDimensionsForMeta(option, meta) {
  const shortEdge = Math.min(option.width, option.height);
  if (meta?.height > meta?.width) {
    return fitWithinSource(
      shortEdge,
      evenFloor((shortEdge * meta.height) / meta.width),
      meta.width,
      meta.height,
    );
  }
  if (meta?.width > meta?.height) {
    return fitWithinSource(
      evenFloor((shortEdge * meta.width) / meta.height),
      shortEdge,
      meta.width,
      meta.height,
    );
  }
  if (meta?.width && meta?.height) {
    return fitWithinSource(shortEdge, shortEdge, meta.width, meta.height);
  }
  return { width: option.width, height: option.height };
}

export function suggestedSelection(meta) {
  if (!meta?.width || !meta?.height) return ["720p"];
  return RESOLUTION_OPTIONS
    .filter((option) => optionFitsSource(option, meta))
    .map((option) => option.value);
}

export function resolutionByValue(value) {
  return RESOLUTION_OPTIONS.find((option) => option.value === value);
}

export function variantDisplayLabel(resolution) {
  return resolutionByValue(resolution)?.label || resolution;
}
