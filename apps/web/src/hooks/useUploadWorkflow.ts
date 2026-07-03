import { useCallback, useEffect, useState } from "react";

import {
  isRequestCanceled,
  requestErrorMessage,
  requestUploadQuote,
  uploadVideo,
} from "../api/client";
import { RESOLUTION_OPTIONS } from "../constants";
import type {
  EncodeSettings,
  SourceVideoMeta,
  UploadQuote,
  UploadQuoteRequest,
  VideoSummary,
} from "../types";
import { classifyResolution, orderedSelection, suggestedSelection } from "../utils/resolutions";

export type VideoCodec = EncodeSettings["video_codec"];

export const DEFAULT_AUDIO_BITRATE_KBPS = 320;

function defaultVideoBitrate(optionKbps: number, codec: VideoCodec): number {
  if (codec === "hevc") return Math.max(64, Math.round((optionKbps * 0.7) / 50) * 50);
  return optionKbps;
}

export function defaultVideoBitrates(codec: VideoCodec): Record<string, number> {
  return Object.fromEntries(
    RESOLUTION_OPTIONS.map((option) => [
      option.value,
      defaultVideoBitrate(option.defaultVideoBitrateKbps, codec),
    ]),
  );
}

export function buildEncodeSettings(
  resolutions: string[],
  videoCodec: VideoCodec,
  videoBitrates: Record<string, number>,
  audioBitrateKbps: number,
): EncodeSettings {
  const video_bitrate_overrides = Object.fromEntries(
    resolutions.map((resolution) => [
      resolution,
      Math.max(64, Math.round(videoBitrates[resolution] || 0)),
    ]),
  );
  return {
    video_codec: videoCodec,
    audio_bitrate_kbps: Math.max(32, Math.round(audioBitrateKbps || DEFAULT_AUDIO_BITRATE_KBPS)),
    video_bitrate_overrides,
  };
}

/**
 * Upload form state: source file with locally-read metadata, titles,
 * privacy flags, rendition selection, and encode settings (with default
 * bitrate migration when the codec changes).
 */
export function useUploadForm() {
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [desc, setDesc] = useState("");
  const [showManifestAddress, setShowManifestAddress] = useState(false);
  const [uploadOriginal, setUploadOriginal] = useState(false);
  const [publishWhenReady, setPublishWhenReady] = useState(false);
  const [selected, setSelected] = useState<string[]>(["720p"]);
  const [videoCodec, setVideoCodec] = useState<VideoCodec>("h264");
  const [audioBitrateKbps, setAudioBitrateKbps] = useState(DEFAULT_AUDIO_BITRATE_KBPS);
  const [videoBitrates, setVideoBitrates] = useState<Record<string, number>>(() =>
    defaultVideoBitrates("h264"),
  );
  const [meta, setMeta] = useState<SourceVideoMeta | null>(null);

  const currentProfile = classifyResolution(meta?.width, meta?.height);

  /** Caller validates the file type; this reads metadata and applies defaults. */
  const inspectFile = useCallback((nextFile: File) => {
    setFile(nextFile);
    setTitle((current) => current || nextFile.name.replace(/\.[^.]+$/, ""));
    setMeta({ loading: true, width: null, height: null, duration: null });

    const objectUrl = URL.createObjectURL(nextFile);
    const video = document.createElement("video");
    video.preload = "metadata";
    video.onloadedmetadata = () => {
      const nextMeta = {
        loading: false,
        width: video.videoWidth,
        height: video.videoHeight,
        duration: video.duration,
        size: nextFile.size,
      };
      setMeta(nextMeta);
      setSelected(suggestedSelection(nextMeta));
      URL.revokeObjectURL(objectUrl);
    };
    video.onerror = () => {
      setMeta({ loading: false, width: null, height: null, duration: null, size: nextFile.size });
      setSelected(["720p"]);
      URL.revokeObjectURL(objectUrl);
    };
    video.src = objectUrl;
  }, []);

  const toggleRes = useCallback((resolution: string) => {
    setSelected((prev) =>
      prev.includes(resolution)
        ? prev.filter((value) => value !== resolution)
        : [...prev, resolution],
    );
  }, []);

  const selectCurrentOnly = useCallback(() => {
    if (currentProfile) setSelected([currentProfile.value]);
  }, [currentProfile]);

  const selectAdaptive = useCallback(() => {
    setSelected(suggestedSelection(meta));
  }, [meta]);

  /** Switch codec, migrating untouched per-resolution defaults to the new codec's defaults. */
  const changeCodec = useCallback(
    (nextCodec: VideoCodec) => {
      const previousCodec = videoCodec;
      setVideoCodec(nextCodec);
      setVideoBitrates((current) => {
        const previousDefaults = defaultVideoBitrates(previousCodec);
        const nextDefaults = defaultVideoBitrates(nextCodec);
        return Object.fromEntries(
          RESOLUTION_OPTIONS.map((option) => {
            const currentValue = current[option.value];
            const nextValue =
              currentValue === previousDefaults[option.value]
                ? nextDefaults[option.value]
                : (currentValue ?? nextDefaults[option.value]);
            return [option.value, nextValue];
          }),
        );
      });
    },
    [videoCodec],
  );

  const setVideoBitrate = useCallback((resolution: string, kbps: number) => {
    setVideoBitrates((current) => ({ ...current, [resolution]: kbps }));
  }, []);

  const reset = useCallback(() => {
    setFile(null);
    setTitle("");
    setDesc("");
    setShowManifestAddress(false);
    setUploadOriginal(false);
    setPublishWhenReady(false);
    setSelected(["720p"]);
    setVideoCodec("h264");
    setAudioBitrateKbps(DEFAULT_AUDIO_BITRATE_KBPS);
    setVideoBitrates(defaultVideoBitrates("h264"));
    setMeta(null);
  }, []);

  return {
    file,
    title,
    setTitle,
    desc,
    setDesc,
    showManifestAddress,
    setShowManifestAddress,
    uploadOriginal,
    setUploadOriginal,
    publishWhenReady,
    setPublishWhenReady,
    selected,
    videoCodec,
    audioBitrateKbps,
    setAudioBitrateKbps,
    videoBitrates,
    meta,
    currentProfile,
    inspectFile,
    toggleRes,
    selectCurrentOnly,
    selectAdaptive,
    changeCodec,
    setVideoBitrate,
    reset,
  };
}

export interface QuoteState {
  data: UploadQuote | null;
  error: string;
  loading: boolean;
}

const EMPTY_QUOTE: QuoteState = { loading: false, error: "", data: null };

interface UploadQuoteInputs {
  audioBitrateKbps: number;
  file: File | null;
  meta: SourceVideoMeta | null;
  selected: string[];
  uploadOriginal: boolean;
  videoBitrates: Record<string, number>;
  videoCodec: VideoCodec;
}

/**
 * Debounced upload price quote: refetches 250ms after any settings change,
 * canceling in-flight requests. `resetQuote` clears the current quote
 * immediately so a stale price never lingers during the debounce window.
 */
export function useUploadQuote({
  file,
  meta,
  selected,
  videoCodec,
  videoBitrates,
  audioBitrateKbps,
  uploadOriginal,
}: UploadQuoteInputs) {
  const [quote, setQuote] = useState<QuoteState>(EMPTY_QUOTE);

  const resetQuote = useCallback(() => {
    setQuote(EMPTY_QUOTE);
  }, []);

  const metaDuration = meta?.duration;
  const metaWidth = meta?.width;
  const metaHeight = meta?.height;

  useEffect(() => {
    if (!file || !metaDuration || !selected.length) {
      setQuote(EMPTY_QUOTE);
      return undefined;
    }

    const controller = new AbortController();
    const timer = setTimeout(async () => {
      const resolutions = orderedSelection(selected);
      setQuote({ loading: true, error: "", data: null });
      try {
        const encodeSettings = buildEncodeSettings(
          resolutions,
          videoCodec,
          videoBitrates,
          audioBitrateKbps,
        );
        const quoteRequest: UploadQuoteRequest = {
          duration_seconds: metaDuration,
          encode_settings: encodeSettings,
          resolutions,
          source_width: metaWidth,
          source_height: metaHeight,
        };
        if (uploadOriginal) {
          quoteRequest.upload_original = true;
          quoteRequest.source_size_bytes = file.size;
        }
        const data = await requestUploadQuote(quoteRequest, controller.signal);
        setQuote({ loading: false, error: "", data });
      } catch (err) {
        if (isRequestCanceled(err)) return;
        setQuote({
          loading: false,
          error: requestErrorMessage(err, "Could not get upload price quote"),
          data: null,
        });
      }
    }, 250);

    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [
    audioBitrateKbps,
    file,
    metaDuration,
    metaWidth,
    metaHeight,
    selected,
    uploadOriginal,
    videoBitrates,
    videoCodec,
  ]);

  return { quote, resetQuote };
}

interface UploadSubmitArgs {
  form: ReturnType<typeof useUploadForm>;
  onSuccess: (video: VideoSummary) => void;
  quote: QuoteState;
}

/** Multipart upload submission with validation and progress reporting. */
export function useUploadSubmit({ form, quote, onSuccess }: UploadSubmitArgs) {
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState(0);

  const submit = useCallback(async () => {
    const { file, title, desc, selected, meta } = form;
    if (!file) return setError("Drop or choose a video file first.");
    if (!title.trim()) return setError("Please enter a title.");
    if (!selected.length) return setError("Select at least one resolution.");
    if (meta?.duration && !quote.data) {
      return setError("Waiting for an upload price quote before starting.");
    }

    setError("");
    setUploading(true);
    setProgress(0);

    const resolutionsToUpload = orderedSelection(selected);

    const fd = new FormData();
    const encodeSettings = buildEncodeSettings(
      resolutionsToUpload,
      form.videoCodec,
      form.videoBitrates,
      form.audioBitrateKbps,
    );
    fd.append("file", file);
    fd.append("title", title.trim());
    fd.append("description", desc.trim());
    fd.append("resolutions", resolutionsToUpload.join(","));
    fd.append("show_original_filename", "false");
    fd.append("show_manifest_address", form.showManifestAddress ? "true" : "false");
    fd.append("upload_original", form.uploadOriginal ? "true" : "false");
    fd.append("publish_when_ready", form.publishWhenReady ? "true" : "false");
    fd.append("video_codec", encodeSettings.video_codec);
    fd.append("audio_bitrate_kbps", String(encodeSettings.audio_bitrate_kbps));
    fd.append("video_bitrate_overrides", JSON.stringify(encodeSettings.video_bitrate_overrides));

    try {
      const data = await uploadVideo(fd, (progressEvent) => {
        if (progressEvent.total) {
          setProgress(Math.round((progressEvent.loaded / progressEvent.total) * 100));
        }
      });
      setProgress(0);
      onSuccess(data);
    } catch (err) {
      setError(requestErrorMessage(err, "Upload failed"));
    } finally {
      setUploading(false);
    }
    return undefined;
  }, [form, onSuccess, quote.data]);

  return { uploading, error, setError, progress, submit };
}
