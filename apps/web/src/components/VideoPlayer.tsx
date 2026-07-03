import { useCallback, useEffect, useRef, useState } from "react";

import { useControlsVisibility } from "../hooks/useControlsVisibility";
import { useHlsPlayer } from "../hooks/useHlsPlayer";
import { STREAM_BASE_URL } from "../runtimeConfig";
import type { VideoVariant } from "../types";
import QualityMenu from "./player/QualityMenu";

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
  const [qualityOpen, setQualityOpen] = useState(false);
  const streamBase = manifestAddress
    ? `${STREAM_BASE_URL}/manifest/${manifestAddress}`
    : `${STREAM_BASE_URL}/${videoId}`;
  const src = `${streamBase}/${resolution}/playlist.m3u8`;

  const { playbackError, capturePlaybackState, playbackIntentRef } = useHlsPlayer(videoRef, src);

  const closeQualityMenu = useCallback(() => setQualityOpen(false), []);
  const {
    controlsActive,
    setControlsActive,
    showControls,
    hideControls,
    scheduleControlsIdleHide,
    clearControlsIdleTimer,
  } = useControlsVisibility(videoRef, closeQualityMenu);

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
  }, [clearControlsIdleTimer, playbackIntentRef, scheduleControlsIdleHide, setControlsActive]);

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
        <QualityMenu
          variants={variants}
          resolution={resolution}
          open={qualityOpen}
          onOpenChange={setQualityOpen}
          onToggle={() => {
            setQualityOpen((open) => !open);
            showControls();
          }}
          onSelect={handleResolutionChange}
        />
      )}
    </div>
  );
}
