import { useCallback, type MutableRefObject } from "react";

import { RESUME_DURATION_TOLERANCE_SECONDS } from "../constants";

const MEDIA_HAVE_FUTURE_DATA = 3;

export interface PlaybackState {
  currentTime: number;
  shouldResume: boolean;
}

interface UseNativeHlsPlaybackOptions {
  playbackIntentRef: MutableRefObject<boolean>;
  playbackStateRef: MutableRefObject<PlaybackState>;
  setPlaybackError: (message: string) => void;
}

export function useNativeHlsPlayback({
  playbackIntentRef,
  playbackStateRef,
  setPlaybackError,
}: UseNativeHlsPlaybackOptions) {
  return useCallback((video: HTMLVideoElement, src: string, isActive: () => boolean) => {
    const { currentTime: resumeAt, shouldResume } = playbackStateRef.current;
    let pendingResumeAt = resumeAt;
    let restorePending = resumeAt > 0;
    let resumedAfterRestore = false;
    let nativeSeekRecoveryTimer: ReturnType<typeof setTimeout> | null = null;
    let nativeSeekResumeAt = 0;
    let nativeSeekShouldResume = false;
    const readCurrentTime = () => (
      Number.isFinite(video.currentTime) ? video.currentTime : 0
    );
    const clearNativeSeekRecoveryTimer = () => {
      if (nativeSeekRecoveryTimer) {
        clearTimeout(nativeSeekRecoveryTimer);
        nativeSeekRecoveryTimer = null;
      }
    };
    const wantsPlayback = () => (
      playbackIntentRef.current || (!video.paused && !video.ended)
    );
    const requestPlayback = () => {
      playbackIntentRef.current = true;
      video.play().catch(() => {});
    };
    const scheduleNativeSeekRecovery = () => {
      clearNativeSeekRecoveryTimer();
      if (!nativeSeekShouldResume) return;

      nativeSeekRecoveryTimer = setTimeout(() => {
        nativeSeekRecoveryTimer = null;
        if (!isActive() || !nativeSeekShouldResume || video.ended) return;

        if (video.paused || video.readyState < MEDIA_HAVE_FUTURE_DATA) {
          try {
            if (
              Math.abs(readCurrentTime() - nativeSeekResumeAt)
              > RESUME_DURATION_TOLERANCE_SECONDS
            ) {
              video.currentTime = nativeSeekResumeAt;
            }
          } catch {
            // Safari can reject seeks while native HLS swaps internal buffers.
          }
          requestPlayback();
          scheduleNativeSeekRecovery();
        }
      }, 750);
    };
    const syncPendingSeek = () => {
      if (!restorePending) return;
      pendingResumeAt = readCurrentTime();
      playbackStateRef.current = { currentTime: pendingResumeAt, shouldResume };
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
          Number.isFinite(duration)
          && duration <= pendingResumeAt + RESUME_DURATION_TOLERANCE_SECONDS
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
    const onNativePlaybackError = () => {
      setPlaybackError("Playback failed because the video segments could not be loaded.");
    };
    const rememberNativeSeekIntent = () => {
      nativeSeekResumeAt = readCurrentTime();
      nativeSeekShouldResume = restorePending
        ? wantsPlayback() || shouldResume
        : wantsPlayback();
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

    return () => {
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
  }, [playbackIntentRef, playbackStateRef, setPlaybackError]);
}
