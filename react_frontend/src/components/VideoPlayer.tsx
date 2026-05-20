import { useCallback, useEffect, useRef, useState, type FocusEvent } from "react";

import { PLAYER_CONTROLS_IDLE_MS } from "../constants";
import { useHlsPlayback } from "../hooks/useHlsPlayback";
import { STREAM_BASE_URL } from "../runtimeConfig";
import type { VideoVariant } from "../types";
import { variantDisplayLabel } from "../utils/resolutions";

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
  const controlsIdleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [qualityOpen, setQualityOpen] = useState(false);
  const [controlsActive, setControlsActive] = useState(true);
  const streamBase = manifestAddress
    ? `${STREAM_BASE_URL}/manifest/${manifestAddress}`
    : `${STREAM_BASE_URL}/${videoId}`;
  const src = `${streamBase}/${resolution}/playlist.m3u8`;
  const selectedLabel = variantDisplayLabel(resolution);
  const {
    capturePlaybackState,
    clearPlaybackIntent,
    markPlaybackIntent,
    playbackError,
  } = useHlsPlayback({
    src,
    videoRef,
  });

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

  const handleResolutionChange = useCallback((nextResolution: string) => {
    capturePlaybackState();
    onResolutionChange(nextResolution);
  }, [capturePlaybackState, onResolutionChange]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return undefined;

    const handlePlay = () => {
      markPlaybackIntent();
      setControlsActive(true);
      scheduleControlsIdleHide();
    };
    const handlePause = () => {
      if (!video.seeking) clearPlaybackIntent();
      clearControlsIdleTimer();
      setControlsActive(true);
    };
    const handleEnded = () => {
      clearPlaybackIntent();
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
  }, [
    clearControlsIdleTimer,
    clearPlaybackIntent,
    markPlaybackIntent,
    scheduleControlsIdleHide,
  ]);

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
