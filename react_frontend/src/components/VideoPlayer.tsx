import { useCallback, useEffect, useRef, useState, type FocusEvent } from "react";
import type Hls from "hls.js";

import { PLAYER_CONTROLS_IDLE_MS, RESUME_DURATION_TOLERANCE_SECONDS } from "../constants";
import { STREAM_BASE_URL } from "../runtimeConfig";
import type { VideoVariant } from "../types";
import { variantDisplayLabel } from "../utils/resolutions";

const HLS_RETRY_CONFIG = {
  fragLoadingMaxRetry: 4,
  fragLoadingMaxRetryTimeout: 8_000,
  fragLoadingRetryDelay: 500,
  levelLoadingMaxRetry: 4,
  levelLoadingMaxRetryTimeout: 8_000,
  levelLoadingRetryDelay: 500,
  manifestLoadingMaxRetry: 4,
  manifestLoadingMaxRetryTimeout: 8_000,
  manifestLoadingRetryDelay: 500,
};
const HLS_FATAL_RECOVERY_ATTEMPTS = 1;

interface PlaybackState {
  currentTime: number;
  shouldResume: boolean;
}

interface VideoPlayerProps {
  manifestAddress?: string | null;
  onResolutionChange: (resolution: string) => void;
  resolution: string;
  variants: VideoVariant[];
  videoId: string;
}

export default function VideoPlayer({
  videoId,
  manifestAddress,
  variants,
  resolution,
  onResolutionChange,
}: VideoPlayerProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const controlsIdleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const playbackStateRef = useRef<PlaybackState>({ currentTime: 0, shouldResume: false });
  const [qualityOpen, setQualityOpen] = useState(false);
  const [controlsActive, setControlsActive] = useState(true);
  const [playbackError, setPlaybackError] = useState("");
  const streamBase = manifestAddress
    ? `${STREAM_BASE_URL}/manifest/${manifestAddress}`
    : `${STREAM_BASE_URL}/${videoId}`;
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

  const handleResolutionChange = useCallback((nextResolution: string) => {
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
        let mediaRecoveryAttempts = 0;
        let networkRecoveryAttempts = 0;
        const hls = new Hls({
          enableWorker: true,
          ...HLS_RETRY_CONFIG,
          lowLatencyMode: false,
          startPosition: resumeAt > 0 ? resumeAt : -1,
        });
        hlsRef.current = hls;
        hls.loadSource(src);
        hls.attachMedia(video);
        hls.on(Hls.Events.MANIFEST_PARSED, resumePlayback);
        hls.on(Hls.Events.ERROR, (_event, data) => {
          if (data?.fatal) {
            if (
              data.type === Hls.ErrorTypes.NETWORK_ERROR
              && networkRecoveryAttempts < HLS_FATAL_RECOVERY_ATTEMPTS
            ) {
              networkRecoveryAttempts += 1;
              hls.startLoad();
              return;
            }

            if (
              data.type === Hls.ErrorTypes.MEDIA_ERROR
              && mediaRecoveryAttempts < HLS_FATAL_RECOVERY_ATTEMPTS
            ) {
              mediaRecoveryAttempts += 1;
              hls.recoverMediaError();
              return;
            }

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
          onBlur={(event: FocusEvent<HTMLDivElement>) => {
            const nextTarget = event.relatedTarget instanceof Node ? event.relatedTarget : null;
            if (!event.currentTarget.contains(nextTarget)) setQualityOpen(false);
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
