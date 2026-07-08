import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

import { PLAYER_CONTROLS_IDLE_MS } from "../constants";

/**
 * Show/hide state for the player overlay controls with an idle-hide timer.
 * `onHide` fires whenever the controls hide (pointer leave or idle timeout)
 * so callers can close popovers such as the quality menu.
 */
export function useControlsVisibility(
  videoRef: RefObject<HTMLVideoElement | null>,
  onHide: () => void,
) {
  const controlsIdleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [controlsActive, setControlsActive] = useState(true);

  const clearControlsIdleTimer = useCallback(() => {
    if (controlsIdleTimerRef.current) {
      clearTimeout(controlsIdleTimerRef.current);
      controlsIdleTimerRef.current = null;
    }
  }, []);

  const hideControls = useCallback(() => {
    clearControlsIdleTimer();
    onHide();
    setControlsActive(false);
  }, [clearControlsIdleTimer, onHide]);

  const scheduleControlsIdleHide = useCallback(() => {
    clearControlsIdleTimer();
    const video = videoRef.current;
    if (!video || video.paused || video.ended) return;

    controlsIdleTimerRef.current = setTimeout(() => {
      onHide();
      setControlsActive(false);
      controlsIdleTimerRef.current = null;
    }, PLAYER_CONTROLS_IDLE_MS);
  }, [clearControlsIdleTimer, onHide, videoRef]);

  const showControls = useCallback(() => {
    setControlsActive(true);
    scheduleControlsIdleHide();
  }, [scheduleControlsIdleHide]);

  useEffect(() => clearControlsIdleTimer, [clearControlsIdleTimer]);

  return {
    controlsActive,
    setControlsActive,
    showControls,
    hideControls,
    scheduleControlsIdleHide,
    clearControlsIdleTimer,
  };
}
