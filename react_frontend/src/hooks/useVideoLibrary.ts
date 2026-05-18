import { useCallback, useEffect, useMemo, useState, type MouseEvent } from "react";
import { useNavigate, useParams } from "react-router-dom";

import {
  approveVideoUpload,
  deleteVideoRecord,
  listVideos,
  requestErrorMessage,
  updateVideoPublication,
  updateVideoVisibility,
} from "../api/client";
import type { VideoSummary, VisibilityUpdate } from "../types";
import { isActiveStatus } from "../utils/status";
import { useAdminCatalogs } from "./useAdminCatalogs";
import { useVideoDetailPolling } from "./useVideoDetailPolling";

interface PlayingState {
  resolution: string;
  videoId: string;
}

export function useVideoLibrary(admin: boolean) {
  const navigate = useNavigate();
  const { videoId: routeVideoId } = useParams<{ videoId?: string }>();
  const detailBasePath = admin ? "/manage" : "/library";
  const [videos, setVideos] = useState<VideoSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [playing, setPlaying] = useState<PlayingState | null>(null);
  const [approving, setApproving] = useState<string | null>(null);
  const [publishing, setPublishing] = useState<string | null>(null);
  const [loadError, setLoadError] = useState("");
  const {
    catalogCopied,
    catalogError,
    catalogPublishing,
    catalogs,
    copyAddress,
    loadCatalogs,
    republishCatalogs,
  } = useAdminCatalogs(admin);
  const {
    actionError,
    clearActionError,
    clearDetail,
    detail,
    detailError,
    loadDetail,
    replaceDetail,
    showActionError,
  } = useVideoDetailPolling({ admin, routeVideoId });
  const detailId = detail?.id;
  const playingVideoId = playing?.videoId;
  const hasActiveVideos = useMemo(
    () => videos.some((video) => isActiveStatus(video.status)),
    [videos],
  );
  const videoCount = videos.length;

  const load = useCallback(async () => {
    try {
      const data = await listVideos({ admin });
      setVideos(data);
      setLoadError("");
    } catch (err) {
      setLoadError(requestErrorMessage(err, "Could not load the video catalog."));
    } finally {
      setLoading(false);
    }
  }, [admin]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const interval = setInterval(() => {
      if (hasActiveVideos) {
        void load();
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [hasActiveVideos, load, videoCount]);

  const openDetail = useCallback(async (videoId: string) => {
    if (routeVideoId === videoId && detailId === videoId) {
      navigate(detailBasePath);
      return;
    }
    if (routeVideoId === videoId) {
      await loadDetail(videoId);
      return;
    }
    navigate(`${detailBasePath}/${encodeURIComponent(videoId)}`);
  }, [detailBasePath, detailId, loadDetail, navigate, routeVideoId]);

  const deleteVideo = useCallback(async (
    videoId: string,
    event: MouseEvent<HTMLButtonElement>,
  ) => {
    event.stopPropagation();
    if (!window.confirm("Delete this video record and remove it from the network catalog?")) return;
    clearActionError();
    try {
      await deleteVideoRecord(videoId);
      setVideos((prev) => prev.filter((video) => video.id !== videoId));
      await loadCatalogs();
      if (detailId === videoId) {
        clearDetail();
        navigate(detailBasePath, { replace: true });
      }
      if (playingVideoId === videoId) setPlaying(null);
    } catch (err) {
      showActionError(requestErrorMessage(err, "Delete failed."));
    }
  }, [
    clearActionError,
    clearDetail,
    detailId,
    detailBasePath,
    loadCatalogs,
    navigate,
    playingVideoId,
    showActionError,
  ]);

  const approveVideo = useCallback(async (videoId: string) => {
    setApproving(videoId);
    clearActionError();
    try {
      const data = await approveVideoUpload(videoId);
      replaceDetail(data);
      await load();
      await loadCatalogs();
    } catch (err) {
      const message = requestErrorMessage(err, "Approval failed.");
      showActionError(message);
      window.alert(message);
    } finally {
      setApproving(null);
    }
  }, [clearActionError, load, loadCatalogs, replaceDetail, showActionError]);

  const updateVisibility = useCallback(async (videoId: string, next: VisibilityUpdate) => {
    clearActionError();
    try {
      const data = await updateVideoVisibility(videoId, next);
      replaceDetail(data);
      setVideos((prev) => prev.map((video) => (
        video.id === videoId ? { ...video, ...data } : video
      )));
      await loadCatalogs();
    } catch (err) {
      showActionError(requestErrorMessage(err, "Visibility update failed."));
    }
  }, [clearActionError, loadCatalogs, replaceDetail, showActionError]);

  const updatePublication = useCallback(async (videoId: string, isPublic: boolean) => {
    setPublishing(videoId);
    clearActionError();
    try {
      const data = await updateVideoPublication(videoId, isPublic);
      replaceDetail(data);
      setVideos((prev) => prev.map((video) => (
        video.id === videoId ? { ...video, ...data } : video
      )));
      await loadCatalogs();
    } catch (err) {
      showActionError(requestErrorMessage(err, isPublic ? "Publish failed." : "Unpublish failed."));
    } finally {
      setPublishing(null);
    }
  }, [clearActionError, loadCatalogs, replaceDetail, showActionError]);

  return {
    actionError,
    approveVideo,
    approving,
    catalogCopied,
    catalogError,
    catalogPublishing,
    catalogs,
    copyAddress,
    deleteVideo,
    detail,
    detailError,
    loadError,
    loading,
    openDetail,
    playing,
    publishing,
    republishCatalogs,
    setPlaying,
    updatePublication,
    updateVisibility,
    videos,
  };
}
