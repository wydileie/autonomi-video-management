import { RESOLUTION_OPTIONS } from "../constants";
import type { ResolutionOption, SourceVideoMeta } from "../types";

type TargetDimensions = {
  width: number;
  height: number;
};

export function orderedSelection(selected: string[]): string[] {
  return RESOLUTION_OPTIONS
    .map((option) => option.value)
    .filter((value) => selected.includes(value));
}

export function classifyResolution(
  width?: number | null,
  height?: number | null,
): ResolutionOption | null {
  if (!width || !height) return null;
  const shortEdge = Math.min(width, height);
  return RESOLUTION_OPTIONS.find((option) => (
    shortEdge >= Math.min(option.width, option.height) * 0.92
  )) || RESOLUTION_OPTIONS[RESOLUTION_OPTIONS.length - 1];
}

export function optionFitsSource(option: ResolutionOption, meta?: SourceVideoMeta | null): boolean {
  if (!meta?.width || !meta?.height) return true;
  const shortEdge = Math.min(meta.width, meta.height);
  return shortEdge >= Math.min(option.width, option.height) * 0.92;
}

function evenFloor(value: number): number {
  const floored = Math.floor(value);
  return Math.max(2, floored - (floored % 2));
}

function fitWithinSource(
  width: number,
  height: number,
  sourceWidth: number,
  sourceHeight: number,
): TargetDimensions {
  if (width <= sourceWidth && height <= sourceHeight) return { width, height };
  const scale = Math.min(sourceWidth / width, sourceHeight / height, 1);
  return {
    width: evenFloor(width * scale),
    height: evenFloor(height * scale),
  };
}

export function targetDimensionsForMeta(
  option: ResolutionOption,
  meta?: SourceVideoMeta | null,
): TargetDimensions {
  const shortEdge = Math.min(option.width, option.height);
  const sourceWidth = meta?.width;
  const sourceHeight = meta?.height;
  if (sourceWidth && sourceHeight && sourceHeight > sourceWidth) {
    return fitWithinSource(
      shortEdge,
      evenFloor((shortEdge * sourceHeight) / sourceWidth),
      sourceWidth,
      sourceHeight,
    );
  }
  if (sourceWidth && sourceHeight && sourceWidth > sourceHeight) {
    return fitWithinSource(
      evenFloor((shortEdge * sourceWidth) / sourceHeight),
      shortEdge,
      sourceWidth,
      sourceHeight,
    );
  }
  if (sourceWidth && sourceHeight) {
    return fitWithinSource(shortEdge, shortEdge, sourceWidth, sourceHeight);
  }
  return { width: option.width, height: option.height };
}

export function suggestedSelection(meta?: SourceVideoMeta | null): string[] {
  if (!meta?.width || !meta?.height) return ["720p"];
  return RESOLUTION_OPTIONS
    .filter((option) => optionFitsSource(option, meta))
    .map((option) => option.value);
}

export function resolutionByValue(value: string): ResolutionOption | undefined {
  return RESOLUTION_OPTIONS.find((option) => option.value === value);
}

export function variantDisplayLabel(resolution: string): string {
  return resolutionByValue(resolution)?.label || resolution;
}
