/* global BigInt */
import React, { useState, useEffect, useRef, useCallback } from "react";
import axios from "axios";
import "./App.css";
import { API_BASE_URL, STREAM_BASE_URL } from "./runtimeConfig";

const API = API_BASE_URL;
const STREAM = STREAM_BASE_URL;
const BRAND_IMAGE = "/autvid-brand.png";
const AUTH_STORAGE_KEY = "autvid_admin_token";
const PLAYER_CONTROLS_IDLE_MS = 2200;
const RESUME_DURATION_TOLERANCE_SECONDS = 0.25;

const RESOLUTION_OPTIONS = [
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

function formatBytes(bytes) {
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

function formatWei(value) {
  try {
    return `${BigInt(value || "0").toLocaleString()} wei`;
  } catch {
    return `${value || "0"} wei`;
  }
}

function orderedSelection(selected) {
  return RESOLUTION_OPTIONS
    .map((option) => option.value)
    .filter((value) => selected.includes(value));
}

function classifyResolution(width, height) {
  if (!width || !height) return null;
  const shortEdge = Math.min(width, height);
  return RESOLUTION_OPTIONS.find((option) => (
    shortEdge >= Math.min(option.width, option.height) * 0.92
  )) || RESOLUTION_OPTIONS[RESOLUTION_OPTIONS.length - 1];
}

function optionFitsSource(option, meta) {
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

function targetDimensionsForMeta(option, meta) {
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

function suggestedSelection(meta) {
  if (!meta?.width || !meta?.height) return ["720p"];
  return RESOLUTION_OPTIONS
    .filter((option) => optionFitsSource(option, meta))
    .map((option) => option.value);
}

function resolutionByValue(value) {
  return RESOLUTION_OPTIONS.find((option) => option.value === value);
}

function isActiveStatus(status) {
  return ["pending", "processing", "awaiting_approval", "uploading"].includes(status);
}

function statusLabel(status) {
  return (status || "").replace(/_/g, " ");
}

function authHeaders(token) {
  return token ? { Authorization: `Bearer ${token}` } : {};
}

function requestErrorMessage(err, fallback) {
  return err?.response?.data?.detail || err.message || fallback;
}

function formatDateTime(value) {
  if (!value) return "";
  return new Date(value).toLocaleString();
}

function variantDisplayLabel(resolution) {
  return resolutionByValue(resolution)?.label || resolution;
}

function VideoPlayer({ videoId, manifestAddress, variants, resolution, onResolutionChange }) {
  const videoRef = useRef(null);
  const hlsRef = useRef(null);
  const controlsIdleTimerRef = useRef(null);
  const playbackStateRef = useRef({ currentTime: 0, shouldResume: false });
  const [qualityOpen, setQualityOpen] = useState(false);
  const [controlsActive, setControlsActive] = useState(true);
  const [playbackError, setPlaybackError] = useState("");
  const streamBase = manifestAddress
    ? `${STREAM}/manifest/${manifestAddress}`
    : `${STREAM}/${videoId}`;
  const src = `${streamBase}/${resolution}/playlist.m3u8`;
  const selectedLabel = variantDisplayLabel(resolution);

  const clearControlsIdleTimer = useCallback(() => {
    if (controlsIdleTimerRef.current) {
      clearTimeout(controlsIdleTimerRef.current);
      controlsIdleTimerRef.current = null;
    }
  }, []);

  const hideControls = useCallback(() => {
    clearControlsIdleTimer();
    setQualityOpen(false);
    setControlsActive(false);
  }, [clearControlsIdleTimer]);

  const scheduleControlsIdleHide = useCallback(() => {
    clearControlsIdleTimer();
    const video = videoRef.current;
    if (!video || video.paused || video.ended) return;

    controlsIdleTimerRef.current = setTimeout(() => {
      setQualityOpen(false);
      setControlsActive(false);
      controlsIdleTimerRef.current = null;
    }, PLAYER_CONTROLS_IDLE_MS);
  }, [clearControlsIdleTimer]);

  const showControls = useCallback(() => {
    setControlsActive(true);
    scheduleControlsIdleHide();
  }, [scheduleControlsIdleHide]);

  const capturePlaybackState = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;

    playbackStateRef.current = {
      currentTime: Number.isFinite(video.currentTime) ? video.currentTime : 0,
      shouldResume: !video.paused && !video.ended,
    };
  }, []);

  const handleResolutionChange = useCallback((nextResolution) => {
    capturePlaybackState();
    onResolutionChange(nextResolution);
  }, [capturePlaybackState, onResolutionChange]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return undefined;

    setPlaybackError("");
    let active = true;
    let cleanupPlayback = () => {};
    const { currentTime: resumeAt, shouldResume } = playbackStateRef.current;
    let resumedAfterRestore = false;
    const resumePlayback = () => {
      if (shouldResume && !resumedAfterRestore) {
        resumedAfterRestore = true;
        video.play().catch(() => {});
      }
    };
    const restoreNativePlayback = () => {
      if (resumeAt > 0) {
        const duration = video.duration;
        if (
          Number.isFinite(duration) &&
          duration <= resumeAt + RESUME_DURATION_TOLERANCE_SECONDS
        ) {
          return;
        }

        try {
          video.currentTime = resumeAt;
        } catch {
          return;
        }
      }

      resumePlayback();
      video.removeEventListener("canplay", restoreNativePlayback);
      video.removeEventListener("durationchange", restoreNativePlayback);
      video.removeEventListener("loadeddata", restoreNativePlayback);
      video.removeEventListener("loadedmetadata", restoreNativePlayback);
    };

    const attachNativePlayback = () => {
      const onNativePlaybackError = () => {
        setPlaybackError("Playback failed because the video segments could not be loaded.");
      };
      video.addEventListener("canplay", restoreNativePlayback);
      video.addEventListener("durationchange", restoreNativePlayback);
      video.addEventListener("loadeddata", restoreNativePlayback);
      video.addEventListener("loadedmetadata", restoreNativePlayback);
      video.addEventListener("error", onNativePlaybackError, { once: true });
      video.src = src;
      video.load();
      cleanupPlayback = () => {
        video.removeEventListener("canplay", restoreNativePlayback);
        video.removeEventListener("durationchange", restoreNativePlayback);
        video.removeEventListener("loadeddata", restoreNativePlayback);
        video.removeEventListener("loadedmetadata", restoreNativePlayback);
        video.removeEventListener("error", onNativePlaybackError);
      };
    };

    import("hls.js").then(({ default: Hls }) => {
      if (!active) return;
      if (Hls.isSupported()) {
        const hls = new Hls({
          enableWorker: true,
          lowLatencyMode: false,
          startPosition: resumeAt > 0 ? resumeAt : -1,
        });
        hlsRef.current = hls;
        hls.loadSource(src);
        hls.attachMedia(video);
        hls.on(Hls.Events.MANIFEST_PARSED, resumePlayback);
        hls.on(Hls.Events.ERROR, (_event, data) => {
          if (data?.fatal) {
            setPlaybackError("Playback failed because the video segments could not be loaded.");
          }
        });
        cleanupPlayback = () => {
          hls.destroy();
          hlsRef.current = null;
        };
        return;
      }

      if (video.canPlayType("application/vnd.apple.mpegurl")) {
        attachNativePlayback();
      }
    }).catch(() => {
      if (active && video.canPlayType("application/vnd.apple.mpegurl")) {
        attachNativePlayback();
      }
    });

    return () => {
      active = false;
      capturePlaybackState();
      cleanupPlayback();
      hlsRef.current = null;
    };
  }, [capturePlaybackState, src]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return undefined;

    const handlePlay = () => {
      setControlsActive(true);
      scheduleControlsIdleHide();
    };
    const handlePause = () => {
      clearControlsIdleTimer();
      setControlsActive(true);
    };

    video.addEventListener("play", handlePlay);
    video.addEventListener("pause", handlePause);
    video.addEventListener("ended", handlePause);
    return () => {
      video.removeEventListener("play", handlePlay);
      video.removeEventListener("pause", handlePause);
      video.removeEventListener("ended", handlePause);
    };
  }, [clearControlsIdleTimer, scheduleControlsIdleHide]);

  useEffect(() => clearControlsIdleTimer, [clearControlsIdleTimer]);

  useEffect(() => {
    setQualityOpen(false);
  }, [resolution]);

  return (
    <div
      className={`player-shell${controlsActive ? " controls-active" : ""}`}
      onFocusCapture={showControls}
      onMouseEnter={showControls}
      onMouseLeave={hideControls}
      onMouseMove={showControls}
      onTouchStart={showControls}
    >
      <video ref={videoRef} className="player" controls playsInline />
      {playbackError && <div className="player-error">{playbackError}</div>}
      {variants.length > 0 && (
        <div
          className={`player-quality${qualityOpen ? " open" : ""}`}
          onMouseLeave={() => setQualityOpen(false)}
          onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) setQualityOpen(false);
          }}
        >
          <button
            type="button"
            className={`quality-toggle${qualityOpen ? " active" : ""}`}
            aria-label="Quality"
            aria-expanded={qualityOpen}
            aria-haspopup="menu"
            onClick={() => {
              setQualityOpen((open) => !open);
              showControls();
            }}
          >
            <span className="gear-icon" aria-hidden="true" />
            <span>{selectedLabel}</span>
          </button>
          {qualityOpen && (
            <div className="quality-menu" role="menu" aria-label="Video quality">
              {variants.map((variant) => {
                const label = variantDisplayLabel(variant.resolution);
                return (
                  <button
                    key={variant.id}
                    type="button"
                    role="menuitemradio"
                    aria-checked={variant.resolution === resolution}
                    className={variant.resolution === resolution ? "active" : ""}
                    onClick={() => handleResolutionChange(variant.resolution)}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function UploadPanel({ token, onUploaded }) {
  const fileInputRef = useRef(null);
  const [file, setFile] = useState(null);
  const [title, setTitle] = useState("");
  const [desc, setDesc] = useState("");
  const [showManifestAddress, setShowManifestAddress] = useState(false);
  const [uploadOriginal, setUploadOriginal] = useState(false);
  const [publishWhenReady, setPublishWhenReady] = useState(false);
  const [selected, setSelected] = useState(["720p"]);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [meta, setMeta] = useState(null);
  const [quote, setQuote] = useState({ loading: false, error: "", data: null });

  const currentProfile = classifyResolution(meta?.width, meta?.height);

  const inspectFile = useCallback((nextFile) => {
    if (!nextFile) return;
    if (!nextFile.type.startsWith("video/")) {
      setError("Please choose a video file.");
      return;
    }

    setError("");
    setFile(nextFile);
    setQuote({ loading: false, error: "", data: null });
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

  useEffect(() => {
    if (!file || !meta?.duration || !selected.length) {
      setQuote({ loading: false, error: "", data: null });
      return undefined;
    }

    const controller = new AbortController();
    const timer = setTimeout(async () => {
      const resolutions = orderedSelection(selected);
      setQuote({ loading: true, error: "", data: null });
      try {
        const quoteRequest = {
          duration_seconds: meta.duration,
          resolutions,
          source_width: meta.width,
          source_height: meta.height,
        };
        if (uploadOriginal) {
          quoteRequest.upload_original = true;
          quoteRequest.source_size_bytes = file.size;
        }
        const res = await axios.post(`${API}/videos/upload/quote`, quoteRequest, {
          headers: authHeaders(token),
          signal: controller.signal,
        });
        setQuote({ loading: false, error: "", data: res.data });
      } catch (err) {
        if (axios.isCancel(err) || err.name === "CanceledError") return;
        setQuote({
          loading: false,
          error: err?.response?.data?.detail || err.message || "Could not get upload price quote",
          data: null,
        });
      }
    }, 250);

    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [file, meta?.duration, meta?.width, meta?.height, selected, token, uploadOriginal]);

  const onDrop = (event) => {
    event.preventDefault();
    setDragging(false);
    inspectFile(event.dataTransfer.files?.[0]);
  };

  const toggleRes = (resolution) => {
    setSelected((prev) => (
      prev.includes(resolution)
        ? prev.filter((value) => value !== resolution)
        : [...prev, resolution]
    ));
  };

  const selectCurrentOnly = () => {
    if (currentProfile) setSelected([currentProfile.value]);
  };

  const selectAdaptive = () => {
    setSelected(suggestedSelection(meta));
  };

  const submit = async (event) => {
    event.preventDefault();
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
    fd.append("file", file);
    fd.append("title", title.trim());
    fd.append("description", desc.trim());
    fd.append("resolutions", resolutionsToUpload.join(","));
    fd.append("show_original_filename", "false");
    fd.append("show_manifest_address", showManifestAddress ? "true" : "false");
    fd.append("upload_original", uploadOriginal ? "true" : "false");
    fd.append("publish_when_ready", publishWhenReady ? "true" : "false");

    try {
      const res = await axios.post(`${API}/videos/upload`, fd, {
        headers: { "Content-Type": "multipart/form-data", ...authHeaders(token) },
        onUploadProgress: (progressEvent) => {
          if (progressEvent.total) {
            setProgress(Math.round((progressEvent.loaded / progressEvent.total) * 100));
          }
        },
      });
      setFile(null);
      setTitle("");
      setDesc("");
      setShowManifestAddress(false);
      setUploadOriginal(false);
      setPublishWhenReady(false);
      setSelected(["720p"]);
      setMeta(null);
      setQuote({ loading: false, error: "", data: null });
      setProgress(0);
      if (fileInputRef.current) fileInputRef.current.value = "";
      onUploaded(res.data);
    } catch (err) {
      setError(err?.response?.data?.detail || err.message || "Upload failed");
    } finally {
      setUploading(false);
    }
    return undefined;
  };

  return (
    <section className="upload-card">
      <div className="section-kicker">Ingest</div>
      <div className="upload-head">
        <div>
          <h1>Drop a video. Build a streaming ladder. Store it on Autonomi.</h1>
          <p>
            The browser reads the source dimensions locally, then we prepare the current
            resolution plus any lower renditions you choose.
          </p>
        </div>
        <div className="network-pill">Local devnet ready</div>
      </div>

      <form onSubmit={submit}>
        <button
          type="button"
          className={`dropzone ${dragging ? "is-dragging" : ""}`}
          onClick={() => fileInputRef.current?.click()}
          onDragEnter={(event) => {
            event.preventDefault();
            setDragging(true);
          }}
          onDragOver={(event) => event.preventDefault()}
          onDragLeave={() => setDragging(false)}
          onDrop={onDrop}
          disabled={uploading}
        >
          <input
            ref={fileInputRef}
            className="hidden-input"
            type="file"
            accept="video/*"
            onChange={(event) => inspectFile(event.target.files?.[0])}
            disabled={uploading}
          />
          <span className="drop-icon">+</span>
          <span className="drop-title">
            {file ? file.name : "Drag and drop a video file"}
          </span>
          <span className="drop-subtitle">
            {file ? `${formatBytes(file.size)} selected` : "or click to browse from your machine"}
          </span>
        </button>

        {file && (
          <div className="source-panel">
            <div>
              <span className="meta-label">Detected source</span>
              <strong>
                {meta?.loading
                  ? "Reading metadata..."
                  : meta?.width
                    ? `${meta.width} x ${meta.height}`
                    : "Resolution unavailable"}
              </strong>
            </div>
            <div>
              <span className="meta-label">Current profile</span>
              <strong>{currentProfile?.label || "Unknown"}</strong>
            </div>
            <div>
              <span className="meta-label">Duration</span>
              <strong>{meta?.duration ? `${Math.round(meta.duration)}s` : "Unknown"}</strong>
            </div>
          </div>
        )}

        <div className="form-grid">
          <label>
            <span>Title</span>
            <input value={title} onChange={(event) => setTitle(event.target.value)} disabled={uploading} />
          </label>
          <label>
            <span>Description</span>
            <input value={desc} onChange={(event) => setDesc(event.target.value)} disabled={uploading} />
          </label>
        </div>

        <div className="privacy-panel">
          <label>
            <input
              type="checkbox"
              checked={showManifestAddress}
              onChange={(event) => setShowManifestAddress(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish manifest address</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={uploadOriginal}
              onChange={(event) => {
                setUploadOriginal(event.target.checked);
                setQuote({ loading: false, error: "", data: null });
              }}
              disabled={uploading}
            />
            <span>Upload original source file</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={publishWhenReady}
              onChange={(event) => setPublishWhenReady(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish automatically when ready</span>
          </label>
        </div>

        <div className="resolution-toolbar">
          <div>
            <span className="meta-label">Renditions to create</span>
            <p>Higher-than-source options are dimmed to avoid accidental upscales.</p>
          </div>
          <div className="quick-actions">
            <button type="button" onClick={selectCurrentOnly} disabled={!currentProfile || uploading}>Current only</button>
            <button type="button" onClick={selectAdaptive} disabled={!file || uploading}>Current + lower</button>
          </div>
        </div>

        <div className="resolution-grid">
          {RESOLUTION_OPTIONS.map((option) => {
            const isCurrent = currentProfile?.value === option.value;
            const disabledBySource = file && !optionFitsSource(option, meta);
            const targetDimensions = targetDimensionsForMeta(option, meta);
            return (
              <button
                key={option.value}
                type="button"
                className={`resolution-card ${selected.includes(option.value) ? "selected" : ""}`}
                onClick={() => !disabledBySource && toggleRes(option.value)}
                disabled={uploading || disabledBySource}
              >
                <span className="resolution-label">{option.label}</span>
                <span>{targetDimensions.width} x {targetDimensions.height}</span>
                <span>{option.bitrate} · {option.note}</span>
                {isCurrent && <strong>Current source profile</strong>}
              </button>
            );
          })}
        </div>

        {file && (
          <div className="quote-panel">
            <div className="quote-main">
              <span className="meta-label">Upload price quote</span>
              {quote.loading && <strong>Quoting Autonomi storage...</strong>}
              {!quote.loading && quote.data && (
                <strong>{formatAttoTokens(quote.data.storage_cost_atto)}</strong>
              )}
              {!quote.loading && !quote.data && (
                <strong>{quote.error ? "Quote unavailable" : "Waiting for video duration"}</strong>
              )}
              <p>
                {quote.data
                  ? `${formatBytes(quote.data.estimated_bytes)} across ${quote.data.segment_count} HLS segments${quote.data.original_file ? ", original file," : ""} and metadata`
                  : quote.error || "The estimate refreshes when renditions change."}
              </p>
            </div>
            {quote.data && (
              <div className="quote-breakdown">
                <span>{formatWei(quote.data.estimated_gas_cost_wei)}</span>
                <span>{quote.data.payment_mode} payment mode</span>
                {quote.data.original_file && <span>{formatBytes(quote.data.original_file.estimated_bytes)} original source</span>}
                {quote.data.sampled && <span>large segment estimate sampled</span>}
              </div>
            )}
          </div>
        )}

        {uploading && (
          <div className="upload-progress">
            <div>
              <span>{progress < 100 ? `Uploading source file ${progress}%` : "Transcoding and preparing final quote..."}</span>
              <span>{selected.map((value) => resolutionByValue(value)?.label || value).join(", ")}</span>
            </div>
            <div className="progress-track"><div style={{ width: `${progress}%` }} /></div>
          </div>
        )}

        {error && <div className="error-box">{error}</div>}

        <button className="primary-action" type="submit" disabled={uploading || quote.loading}>
          {uploading ? "Creating final quote..." : "Upload source"}
        </button>
      </form>
    </section>
  );
}

function FinalQuotePanel({ quote, expiresAt, onApprove, approving }) {
  if (!quote) {
    return <p className="muted">Preparing the final quote from transcoded segments...</p>;
  }
  const originalBytes = quote.original_file?.byte_size || quote.original_file?.estimated_bytes || 0;
  const transcodedBytes = quote.actual_transcoded_bytes || quote.actual_media_bytes || quote.estimated_bytes;

  return (
    <div className="quote-panel final-quote-panel">
      <div className="quote-main">
        <span className="meta-label">Final Autonomi quote</span>
        <strong>{formatAttoTokens(quote.storage_cost_atto)}</strong>
        <p>
          {originalBytes
            ? `${formatBytes(transcodedBytes)} transcoded media plus ${formatBytes(originalBytes)} original source`
            : `${formatBytes(transcodedBytes)} of transcoded media`}
          across {quote.segment_count} HLS segments. Approval expires {formatDateTime(expiresAt || quote.approval_expires_at)}.
        </p>
      </div>
      <div className="quote-breakdown">
        <span>{formatWei(quote.estimated_gas_cost_wei)}</span>
        <span>{formatBytes(quote.metadata_bytes)} metadata estimate</span>
        {originalBytes > 0 && <span>{formatBytes(originalBytes)} original source</span>}
        <span>{quote.payment_mode} payment mode</span>
      </div>
      <button type="button" className="approve-action" onClick={onApprove} disabled={approving}>
        {approving ? "Approving..." : "Approve upload"}
      </button>
    </div>
  );
}

function LoginPanel({ onLogin }) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const submit = async (event) => {
    event.preventDefault();
    setLoading(true);
    setError("");
    try {
      const res = await axios.post(`${API}/auth/login`, { username, password });
      onLogin(res.data);
    } catch (err) {
      setError(err?.response?.data?.detail || err.message || "Login failed");
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className="login-card">
      <div className="login-grid">
        <div>
          <div className="section-kicker">Admin</div>
          <h1>Sign in to manage uploads.</h1>
          <form onSubmit={submit} className="login-form">
            <label>
              <span>Username</span>
              <input value={username} onChange={(event) => setUsername(event.target.value)} disabled={loading} />
            </label>
            <label>
              <span>Password</span>
              <input type="password" value={password} onChange={(event) => setPassword(event.target.value)} disabled={loading} />
            </label>
            {error && <div className="error-box">{error}</div>}
            <button className="primary-action" type="submit" disabled={loading}>
              {loading ? "Signing in..." : "Sign in"}
            </button>
          </form>
        </div>
        <div className="login-brand-panel" aria-hidden="true">
          <img src={BRAND_IMAGE} alt="" />
        </div>
      </div>
    </section>
  );
}

function Library({ admin = false, token = "" }) {
  const [videos, setVideos] = useState([]);
  const [loading, setLoading] = useState(true);
  const [playing, setPlaying] = useState(null);
  const [detail, setDetail] = useState(null);
  const [approving, setApproving] = useState(null);
  const [publishing, setPublishing] = useState(null);
  const [loadError, setLoadError] = useState("");
  const [detailError, setDetailError] = useState("");
  const [actionError, setActionError] = useState("");
  const activeDetailId = detail?.id;
  const activeDetailStatus = detail?.status;

  const load = useCallback(async () => {
    try {
      const res = await axios.get(`${API}${admin ? "/admin" : ""}/videos`, {
        headers: authHeaders(token),
      });
      setVideos(res.data);
      setLoadError("");
    } catch (err) {
      setLoadError(requestErrorMessage(err, "Could not load the video catalog."));
    } finally {
      setLoading(false);
    }
  }, [admin, token]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    const interval = setInterval(() => {
      if (videos.some((video) => isActiveStatus(video.status))) {
        load();
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [videos, load]);

  useEffect(() => {
    if (!activeDetailId || !isActiveStatus(activeDetailStatus)) return undefined;
    const interval = setInterval(async () => {
      try {
        const res = await axios.get(`${API}${admin ? "/admin" : ""}/videos/${activeDetailId}`, {
          headers: authHeaders(token),
        });
        setDetail(res.data);
        setDetailError("");
      } catch (err) {
        setDetailError(requestErrorMessage(err, "Could not refresh video details."));
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [activeDetailId, activeDetailStatus, admin, token]);

  const openDetail = async (videoId) => {
    if (detail?.id === videoId) {
      setDetail(null);
      return;
    }
    setDetailError("");
    setActionError("");
    try {
      const res = await axios.get(`${API}${admin ? "/admin" : ""}/videos/${videoId}`, {
        headers: authHeaders(token),
      });
      setDetail(res.data);
    } catch (err) {
      setDetailError(requestErrorMessage(err, "Could not load video details."));
    }
  };

  const deleteVideo = async (videoId, event) => {
    event.stopPropagation();
    if (!window.confirm("Delete this video record and remove it from the network catalog?")) return;
    setActionError("");
    try {
      await axios.delete(`${API}/admin/videos/${videoId}`, { headers: authHeaders(token) });
      setVideos((prev) => prev.filter((video) => video.id !== videoId));
      if (detail?.id === videoId) setDetail(null);
      if (playing?.videoId === videoId) setPlaying(null);
    } catch (err) {
      setActionError(requestErrorMessage(err, "Delete failed."));
    }
  };

  const approveVideo = async (videoId) => {
    setApproving(videoId);
    setActionError("");
    try {
      const res = await axios.post(`${API}/admin/videos/${videoId}/approve`, null, {
        headers: authHeaders(token),
      });
      setDetail(res.data);
      await load();
    } catch (err) {
      const message = requestErrorMessage(err, "Approval failed.");
      setActionError(message);
      window.alert(message);
    } finally {
      setApproving(null);
    }
  };

  const updateVisibility = async (videoId, next) => {
    setActionError("");
    try {
      const res = await axios.patch(`${API}/admin/videos/${videoId}/visibility`, next, {
        headers: authHeaders(token),
      });
      setDetail(res.data);
      setVideos((prev) => prev.map((video) => (video.id === videoId ? { ...video, ...res.data } : video)));
    } catch (err) {
      setActionError(requestErrorMessage(err, "Visibility update failed."));
    }
  };

  const updatePublication = async (videoId, isPublic) => {
    setPublishing(videoId);
    setActionError("");
    try {
      const res = await axios.patch(`${API}/admin/videos/${videoId}/publication`, { is_public: isPublic }, {
        headers: authHeaders(token),
      });
      setDetail(res.data);
      setVideos((prev) => prev.map((video) => (video.id === videoId ? { ...video, ...res.data } : video)));
    } catch (err) {
      setActionError(requestErrorMessage(err, isPublic ? "Publish failed." : "Unpublish failed."));
    } finally {
      setPublishing(null);
    }
  };

  if (loading) {
    return (
      <div className="empty-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>Loading the catalog...</strong>
      </div>
    );
  }
  if (loadError && !videos.length) {
    return (
      <div className="empty-state error-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>{loadError}</strong>
      </div>
    );
  }
  if (!videos.length) {
    return (
      <div className="empty-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>
          {admin ? "No videos yet. Upload one to build your first stream." : "No videos are available yet."}
        </strong>
      </div>
    );
  }

  return (
    <section className="library-card">
      <div className="section-kicker">Catalog</div>
      <div className="library-head">
        <div>
          <h2>AutVid library</h2>
          <p>{admin ? "Manage processing, publishing, and public metadata." : "Browse published streams."}</p>
        </div>
        <span>{videos.length} video{videos.length === 1 ? "" : "s"}</span>
      </div>
      {loadError && <div className="error-box">{loadError}</div>}
      {detailError && <div className="error-box">{detailError}</div>}
      {actionError && <div className="error-box">{actionError}</div>}

      <div className="video-list">
        {videos.map((video) => (
          <article className="video-row" key={video.id}>
            <button type="button" className="video-summary" onClick={() => openDetail(video.id)}>
              <div className="video-title">
                <span className="video-thumb" aria-hidden="true">
                  <span />
                </span>
                <div>
                  <strong>{video.title}</strong>
                  <span>{video.original_filename || video.description || "Published stream"}</span>
                </div>
              </div>
              <div className="row-meta">
                {admin && (
                  <span className={`status ${video.is_public ? "public" : "private"}`}>
                    {video.is_public ? "public" : "hidden"}
                  </span>
                )}
                <span className={`status ${video.status}`}>{statusLabel(video.status)}</span>
                <span>{new Date(video.created_at).toLocaleDateString()}</span>
              </div>
            </button>

            {detail?.id === video.id && (
              <div className="video-detail">
                {admin && detail.status === "awaiting_approval" ? (
                  <FinalQuotePanel
                    quote={detail.final_quote}
                    expiresAt={detail.approval_expires_at}
                    approving={approving === video.id}
                    onApprove={() => approveVideo(video.id)}
                  />
                ) : admin && detail.status === "uploading" ? (
                  <p className="muted">Uploading approved segments and publishing the network manifest...</p>
                ) : admin && (detail.status === "processing" || detail.status === "pending") ? (
                  <p className="muted">Processing renditions and preparing the final quote...</p>
                ) : admin && (detail.status === "error" || detail.status === "expired") ? (
                  <p className="muted">{detail.error_message || "This video could not be completed."}</p>
                ) : detail.variants.length === 0 ? (
                  <p className="muted">No variants available.</p>
                ) : (
                  <>
                    {(() => {
                      const selectedVariant = detail.variants.find((variant) => (
                        playing?.videoId === video.id && playing?.resolution === variant.resolution
                      )) || detail.variants[0];

                      return (
                        <VideoPlayer
                          videoId={video.id}
                          manifestAddress={admin ? detail.manifest_address : null}
                          variants={detail.variants}
                          resolution={selectedVariant.resolution}
                          onResolutionChange={(nextResolution) => setPlaying({
                            videoId: video.id,
                            resolution: nextResolution,
                          })}
                        />
                      );
                    })()}
                  </>
                )}
                {admin && (
                  <>
                    {detail.status === "ready" && (
                      <div className="publication-panel">
                        <button
                          type="button"
                          className={detail.is_public ? "secondary-action" : "primary-action compact-action"}
                          disabled={publishing === video.id}
                          onClick={() => updatePublication(video.id, !detail.is_public)}
                        >
                          {publishing === video.id
                            ? "Updating..."
                            : detail.is_public ? "Unpublish" : "Publish"}
                        </button>
                        <span className={`status ${detail.is_public ? "public" : "private"}`}>
                          {detail.is_public ? "public" : "hidden"}
                        </span>
                      </div>
                    )}
                    <div className="visibility-panel">
                      <label>
                        <input
                          type="checkbox"
                          checked={!!detail.show_manifest_address}
                          onChange={(event) => updateVisibility(video.id, {
                            show_original_filename: false,
                            show_manifest_address: event.target.checked,
                          })}
                        />
                        <span>Publish manifest address</span>
                      </label>
                    </div>
                  </>
                )}
                {(admin || detail.manifest_address) && (
                  <div className="detail-footer">
                    <div className="detail-addresses">
                      <code>{detail.manifest_address || "Manifest hidden or pending"}</code>
                      {admin && detail.original_file_address && (
                        <code>Original source: {detail.original_file_address}</code>
                      )}
                    </div>
                    {admin && (
                      <button type="button" className="danger-action" onClick={(event) => deleteVideo(video.id, event)}>
                        Delete
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}

export default function App() {
  const [tab, setTab] = useState("library");
  const [refreshKey, setRefreshKey] = useState(0);
  const [auth, setAuth] = useState(() => {
    const token = window.localStorage.getItem(AUTH_STORAGE_KEY);
    return token ? { access_token: token, username: "" } : null;
  });

  useEffect(() => {
    if (!auth?.access_token) return undefined;
    let active = true;
    axios.get(`${API}/auth/me`, { headers: authHeaders(auth.access_token) })
      .then((res) => {
        if (active) setAuth((current) => ({ ...current, username: res.data.username }));
      })
      .catch(() => {
        window.localStorage.removeItem(AUTH_STORAGE_KEY);
        if (active) {
          setAuth(null);
          setTab("library");
        }
      });
    return () => {
      active = false;
    };
  }, [auth?.access_token]);

  const handleLogin = (nextAuth) => {
    window.localStorage.setItem(AUTH_STORAGE_KEY, nextAuth.access_token);
    setAuth(nextAuth);
    setTab("manage");
  };

  const logout = () => {
    window.localStorage.removeItem(AUTH_STORAGE_KEY);
    setAuth(null);
    setTab("library");
  };

  const handleUploaded = () => {
    setRefreshKey((value) => value + 1);
    setTab("manage");
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand">
          <img className="brand-image" src={BRAND_IMAGE} alt="AutVid: Autonomi Video Vault" />
          <div className="brand-summary">
            <p>Smooth, adaptive video streaming powered by Autonomi.</p>
          </div>
        </div>
        <div className="topbar-actions">
          {auth && <span className="user-pill">{auth.username || "Admin"}</span>}
          <nav aria-label="Primary">
            <button type="button" className={tab === "library" ? "active" : ""} onClick={() => setTab("library")}>Library</button>
            {auth ? (
              <>
                <button type="button" className={tab === "manage" ? "active" : ""} onClick={() => setTab("manage")}>Manage</button>
                <button type="button" className={tab === "upload" ? "active" : ""} onClick={() => setTab("upload")}>Upload</button>
                <button type="button" onClick={logout}>Logout</button>
              </>
            ) : (
              <button type="button" className={tab === "login" ? "active" : ""} onClick={() => setTab("login")}>Login</button>
            )}
          </nav>
        </div>
      </header>

      <main className="workspace-main">
        {tab === "upload" && auth && <UploadPanel token={auth.access_token} onUploaded={handleUploaded} />}
        {tab === "manage" && auth && <Library key={`admin-${refreshKey}`} admin token={auth.access_token} />}
        {tab === "login" && !auth && <LoginPanel onLogin={handleLogin} />}
        {tab === "library" && <Library key={`public-${refreshKey}`} />}
      </main>
    </div>
  );
}
