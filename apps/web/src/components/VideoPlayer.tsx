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
const MEDIA_HAVE_FUTURE_DATA = 3;

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
  const playbackIntentRef = useRef(false);
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

  const capturePlaybackStateFromVideo = useCallback((video: HTMLVideoElement) => {
    playbackStateRef.current = {
      currentTime: Number.isFinite(video.currentTime) ? video.currentTime : 0,
      shouldResume: playbackIntentRef.current || (!video.paused && !video.ended),
    };
  }, []);

  const capturePlaybackState = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    capturePlaybackStateFromVideo(video);
  }, [capturePlaybackStateFromVideo]);

  const handleResolutionChange = useCallback(
    (nextResolution: string) => {
      capturePlaybackState();
      onResolutionChange(nextResolution);
    },
    [capturePlaybackState, onResolutionChange],
  );

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return undefined;

    setPlaybackError("");
    let active = true;
    let cleanupPlayback = () => {};
    const { currentTime: resumeAt, shouldResume } = playbackStateRef.current;
    let pendingResumeAt = resumeAt;
    let restorePending = resumeAt > 0;
    let hlsManifestParsed = false;
    let resumedAfterRestore = false;
    let suppressNextHlsSeekSync = false;
    let nativeSeekRecoveryTimer: ReturnType<typeof setTimeout> | null = null;
    let nativeSeekResumeAt = 0;
    let nativeSeekShouldResume = false;
    const readCurrentTime = () => (Number.isFinite(video.currentTime) ? video.currentTime : 0);
    const seekVideoTo = (targetTime: number) => {
      if (targetTime <= 0) return;

      const duration = video.duration;
      if (Number.isFinite(duration) && duration <= targetTime + RESUME_DURATION_TOLERANCE_SECONDS) {
        return;
      }

      if (Math.abs(readCurrentTime() - targetTime) <= RESUME_DURATION_TOLERANCE_SECONDS) {
        return;
      }

      try {
        suppressNextHlsSeekSync = true;
        video.currentTime = targetTime;
      } catch {
        suppressNextHlsSeekSync = false;
        // MediaSource can briefly reject seeks while hls.js is rebuilding buffers.
      }
    };
    const clearNativeSeekRecoveryTimer = () => {
      if (nativeSeekRecoveryTimer) {
        clearTimeout(nativeSeekRecoveryTimer);
        nativeSeekRecoveryTimer = null;
      }
    };
    const wantsPlayback = () => playbackIntentRef.current || (!video.paused && !video.ended);
    const requestPlayback = () => {
      playbackIntentRef.current = true;
      video.play().catch(() => {});
    };
    const scheduleNativeSeekRecovery = () => {
      clearNativeSeekRecoveryTimer();
      if (!nativeSeekShouldResume) return;

      nativeSeekRecoveryTimer = setTimeout(() => {
        nativeSeekRecoveryTimer = null;
        if (!active || !nativeSeekShouldResume || video.ended) return;

        if (video.paused || video.readyState < MEDIA_HAVE_FUTURE_DATA) {
          try {
            if (
              Math.abs(readCurrentTime() - nativeSeekResumeAt) > RESUME_DURATION_TOLERANCE_SECONDS
            ) {
              video.currentTime = nativeSeekResumeAt;
            }
          } catch {
            // Safari can reject seeks while native HLS is swapping internal buffers.
          }
          requestPlayback();
          scheduleNativeSeekRecovery();
        }
      }, 750);
    };
    const syncPendingSeek = () => {
      if (!restorePending) return;
      if (suppressNextHlsSeekSync) {
        suppressNextHlsSeekSync = false;
        return;
      }
      pendingResumeAt = readCurrentTime();
      playbackStateRef.current = { currentTime: pendingResumeAt, shouldResume };

      const hls = hlsRef.current;
      if (hls && hlsManifestParsed) {
        hls.startLoad(pendingResumeAt > 0 ? pendingResumeAt : 0);
      }
    };
    const finishRestore = () => {
      restorePending = false;
      video.removeEventListener("canplay", finishRestore);
      video.removeEventListener("playing", finishRestore);
    };
    const resumePlayback = () => {
      if (shouldResume && !resumedAfterRestore) {
        resumedAfterRestore = true;
        requestPlayback();
      }
    };
    const removeNativeRestoreListeners = () => {
      video.removeEventListener("canplay", restoreNativePlayback);
      video.removeEventListener("durationchange", restoreNativePlayback);
      video.removeEventListener("loadeddata", restoreNativePlayback);
      video.removeEventListener("loadedmetadata", restoreNativePlayback);
      video.removeEventListener("seeking", syncPendingSeek);
    };
    const restoreNativePlayback = () => {
      if (pendingResumeAt > 0) {
        const duration = video.duration;
        if (
          Number.isFinite(duration) &&
          duration <= pendingResumeAt + RESUME_DURATION_TOLERANCE_SECONDS
        ) {
          return;
        }

        try {
          video.currentTime = pendingResumeAt;
        } catch {
          return;
        }
      }

      finishRestore();
      resumePlayback();
      removeNativeRestoreListeners();
    };

    const attachNativePlayback = () => {
      const onNativePlaybackError = () => {
        setPlaybackError("Playback failed because the video segments could not be loaded.");
      };
      const rememberNativeSeekIntent = () => {
        nativeSeekResumeAt = readCurrentTime();
        nativeSeekShouldResume = restorePending ? wantsPlayback() || shouldResume : wantsPlayback();
        if (nativeSeekShouldResume) scheduleNativeSeekRecovery();
      };
      const resumeAfterNativeSeek = () => {
        nativeSeekResumeAt = readCurrentTime();
        if (!nativeSeekShouldResume) return;
        requestPlayback();
        scheduleNativeSeekRecovery();
      };
      const finishNativeSeekRecovery = () => {
        nativeSeekShouldResume = false;
        clearNativeSeekRecoveryTimer();
      };
      const recoverNativeSeekStall = () => {
        if (!nativeSeekShouldResume && !wantsPlayback()) return;
        nativeSeekResumeAt = readCurrentTime();
        nativeSeekShouldResume = true;
        scheduleNativeSeekRecovery();
      };
      const cancelNativeSeekRecovery = () => {
        if (video.seeking) return;
        nativeSeekShouldResume = false;
        clearNativeSeekRecoveryTimer();
      };
      video.addEventListener("canplay", restoreNativePlayback);
      video.addEventListener("durationchange", restoreNativePlayback);
      video.addEventListener("loadeddata", restoreNativePlayback);
      video.addEventListener("loadedmetadata", restoreNativePlayback);
      video.addEventListener("seeking", syncPendingSeek);
      video.addEventListener("seeking", rememberNativeSeekIntent);
      video.addEventListener("seeked", resumeAfterNativeSeek);
      video.addEventListener("canplay", resumeAfterNativeSeek);
      video.addEventListener("playing", finishNativeSeekRecovery);
      video.addEventListener("pause", cancelNativeSeekRecovery);
      video.addEventListener("stalled", recoverNativeSeekStall);
      video.addEventListener("waiting", recoverNativeSeekStall);
      video.addEventListener("error", onNativePlaybackError, { once: true });
      video.src = src;
      video.load();
      cleanupPlayback = () => {
        clearNativeSeekRecoveryTimer();
        removeNativeRestoreListeners();
        video.removeEventListener("seeking", rememberNativeSeekIntent);
        video.removeEventListener("seeked", resumeAfterNativeSeek);
        video.removeEventListener("canplay", resumeAfterNativeSeek);
        video.removeEventListener("playing", finishNativeSeekRecovery);
        video.removeEventListener("pause", cancelNativeSeekRecovery);
        video.removeEventListener("stalled", recoverNativeSeekStall);
        video.removeEventListener("waiting", recoverNativeSeekStall);
        video.removeEventListener("error", onNativePlaybackError);
        video.removeAttribute("src");
        video.load();
      };
    };

    import("hls.js")
      .then(({ default: Hls }) => {
        if (!active) return;
        if (Hls.isSupported()) {
          let mediaRecoveryAttempts = 0;
          let networkRecoveryAttempts = 0;
          const hls = new Hls({
            enableWorker: true,
            ...HLS_RETRY_CONFIG,
            autoStartLoad: resumeAt > 0 ? false : true,
            lowLatencyMode: false,
            startPosition: resumeAt > 0 ? resumeAt : -1,
          });
          hlsRef.current = hls;
          hls.loadSource(src);
          hls.attachMedia(video);
          video.addEventListener("canplay", finishRestore);
          video.addEventListener("playing", finishRestore);
          video.addEventListener("seeking", syncPendingSeek);
          const hlsLoadPosition = () => {
            const currentTime = readCurrentTime();
            if (
              restorePending &&
              pendingResumeAt > RESUME_DURATION_TOLERANCE_SECONDS &&
              currentTime <= RESUME_DURATION_TOLERANCE_SECONDS
            ) {
              return pendingResumeAt;
            }
            return currentTime > 0 ? currentTime : 0;
          };
          const recoverHlsPlayback = () => {
            if (!active || video.ended || video.paused || !wantsPlayback()) return;
            const loadPosition = hlsLoadPosition();
            hls.startLoad(loadPosition);
            seekVideoTo(loadPosition);
          };
          video.addEventListener("stalled", recoverHlsPlayback);
          video.addEventListener("waiting", recoverHlsPlayback);
          hls.on(Hls.Events.MANIFEST_PARSED, () => {
            hlsManifestParsed = true;
            if (restorePending) {
              const loadPosition = hlsLoadPosition();
              hls.startLoad(loadPosition);
              seekVideoTo(loadPosition);
            }
            resumePlayback();
          });
          hls.on(Hls.Events.ERROR, (_event, data) => {
            if (data?.fatal) {
              if (
                data.type === Hls.ErrorTypes.NETWORK_ERROR &&
                networkRecoveryAttempts < HLS_FATAL_RECOVERY_ATTEMPTS
              ) {
                networkRecoveryAttempts += 1;
                const loadPosition = hlsLoadPosition();
                hls.startLoad(loadPosition);
                seekVideoTo(loadPosition);
                return;
              }

              if (
                data.type === Hls.ErrorTypes.MEDIA_ERROR &&
                mediaRecoveryAttempts < HLS_FATAL_RECOVERY_ATTEMPTS
              ) {
                mediaRecoveryAttempts += 1;
                seekVideoTo(hlsLoadPosition());
                hls.recoverMediaError();
                return;
              }

              setPlaybackError("Playback failed because the video segments could not be loaded.");
            }
          });
          cleanupPlayback = () => {
            video.removeEventListener("canplay", finishRestore);
            video.removeEventListener("playing", finishRestore);
            video.removeEventListener("seeking", syncPendingSeek);
            video.removeEventListener("stalled", recoverHlsPlayback);
            video.removeEventListener("waiting", recoverHlsPlayback);
            hls.destroy();
            hlsRef.current = null;
          };
          return;
        }

        if (video.canPlayType("application/vnd.apple.mpegurl")) {
          attachNativePlayback();
        }
      })
      .catch(() => {
        if (active && video.canPlayType("application/vnd.apple.mpegurl")) {
          attachNativePlayback();
        }
      });

    return () => {
      active = false;
      capturePlaybackStateFromVideo(video);
      cleanupPlayback();
      hlsRef.current = null;
    };
  }, [capturePlaybackStateFromVideo, src]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return undefined;

    const handlePlay = () => {
      playbackIntentRef.current = true;
      setControlsActive(true);
      scheduleControlsIdleHide();
    };
    const handlePause = () => {
      if (!video.seeking) playbackIntentRef.current = false;
      clearControlsIdleTimer();
      setControlsActive(true);
    };
    const handleEnded = () => {
      playbackIntentRef.current = false;
      handlePause();
    };

    video.addEventListener("play", handlePlay);
    video.addEventListener("pause", handlePause);
    video.addEventListener("ended", handleEnded);
    return () => {
      video.removeEventListener("play", handlePlay);
      video.removeEventListener("pause", handlePause);
      video.removeEventListener("ended", handleEnded);
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
