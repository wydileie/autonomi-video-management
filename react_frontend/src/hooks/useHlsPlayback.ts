import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import type Hls from "hls.js";

import { useNativeHlsPlayback, type PlaybackState } from "./useNativeHlsPlayback";

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

interface UseHlsPlaybackOptions {
  src: string;
  videoRef: RefObject<HTMLVideoElement>;
}

export function useHlsPlayback({ src, videoRef }: UseHlsPlaybackOptions) {
  const hlsRef = useRef<Hls | null>(null);
  const playbackStateRef = useRef<PlaybackState>({ currentTime: 0, shouldResume: false });
  const playbackIntentRef = useRef(false);
  const [playbackError, setPlaybackError] = useState("");
  const attachNativePlayback = useNativeHlsPlayback({
    playbackIntentRef,
    playbackStateRef,
    setPlaybackError,
  });

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
  }, [capturePlaybackStateFromVideo, videoRef]);

  const markPlaybackIntent = useCallback(() => {
    playbackIntentRef.current = true;
  }, []);

  const clearPlaybackIntent = useCallback(() => {
    playbackIntentRef.current = false;
  }, []);

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
    const readCurrentTime = () => (
      Number.isFinite(video.currentTime) ? video.currentTime : 0
    );
    const requestPlayback = () => {
      playbackIntentRef.current = true;
      video.play().catch(() => {});
    };
    const syncPendingSeek = () => {
      if (!restorePending) return;
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
    const attachHlsPlayback = (HlsConstructor: typeof Hls) => {
      let mediaRecoveryAttempts = 0;
      let networkRecoveryAttempts = 0;
      const hls = new HlsConstructor({
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
      hls.on(HlsConstructor.Events.MANIFEST_PARSED, () => {
        hlsManifestParsed = true;
        if (restorePending) {
          hls.startLoad(pendingResumeAt > 0 ? pendingResumeAt : 0);
        }
        resumePlayback();
      });
      hls.on(HlsConstructor.Events.ERROR, (_event, data) => {
        if (data?.fatal) {
          if (
            data.type === HlsConstructor.ErrorTypes.NETWORK_ERROR
            && networkRecoveryAttempts < HLS_FATAL_RECOVERY_ATTEMPTS
          ) {
            networkRecoveryAttempts += 1;
            hls.startLoad();
            return;
          }

          if (
            data.type === HlsConstructor.ErrorTypes.MEDIA_ERROR
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
        video.removeEventListener("canplay", finishRestore);
        video.removeEventListener("playing", finishRestore);
        video.removeEventListener("seeking", syncPendingSeek);
        hls.destroy();
        hlsRef.current = null;
      };
    };
    const attachNativeFallback = () => {
      if (video.canPlayType("application/vnd.apple.mpegurl")) {
        cleanupPlayback = attachNativePlayback(video, src, () => active);
      }
    };

    import("hls.js").then(({ default: HlsConstructor }) => {
      if (!active) return;
      if (HlsConstructor.isSupported()) {
        attachHlsPlayback(HlsConstructor);
        return;
      }

      attachNativeFallback();
    }).catch(() => {
      if (active) attachNativeFallback();
    });

    return () => {
      active = false;
      capturePlaybackStateFromVideo(video);
      cleanupPlayback();
      hlsRef.current = null;
    };
  }, [attachNativePlayback, capturePlaybackStateFromVideo, src, videoRef]);

  return {
    capturePlaybackState,
    clearPlaybackIntent,
    markPlaybackIntent,
    playbackError,
  };
}
