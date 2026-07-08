import { useCallback, useEffect, useState } from "react";

import {
  approveVideoUpload,
  deleteVideoRecord,
  getAdminCatalogs,
  getVideoDetails,
  listVideos,
  publishAdminCatalogs,
  requestErrorMessage,
  updateVideoPublication,
  updateVideoVisibility,
} from "../api/client";
import type { AdminCatalogs, VideoDetail, VideoSummary, VisibilityUpdate } from "../types";
import { isActiveStatus } from "../utils/status";

/** Video list with initial load and 5s polling while any video is active. */
export function useLibraryData(admin: boolean) {
  const [videos, setVideos] = useState<VideoSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");

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

  return { videos, setVideos, loading, loadError, load };
}

/** Route-driven video detail with 5s polling while the video is active. */
export function useVideoDetail(admin: boolean, routeVideoId: string | undefined) {
  const [detail, setDetail] = useState<VideoDetail | null>(null);
  const [detailError, setDetailError] = useState("");
  const activeDetailId = detail?.id;
  const activeDetailStatus = detail?.status;

  const loadDetail = useCallback(
    async (videoId: string) => {
      setDetailError("");
      try {
        const data = await getVideoDetails({ admin, videoId });
        setDetail(data);
      } catch (err) {
        setDetailError(requestErrorMessage(err, "Could not load video details."));
      }
    },
    [admin],
  );

  useEffect(() => {
    if (!routeVideoId) {
      setDetail(null);
      return;
    }

    let active = true;
    setDetailError("");
    getVideoDetails({ admin, videoId: routeVideoId })
      .then((data) => {
        if (active) setDetail(data);
      })
      .catch((err) => {
        if (active) {
          setDetail(null);
          setDetailError(requestErrorMessage(err, "Could not load video details."));
        }
      });

    return () => {
      active = false;
    };
  }, [admin, routeVideoId]);

  useEffect(() => {
    if (!activeDetailId || !isActiveStatus(activeDetailStatus)) return undefined;
    const interval = setInterval(async () => {
      try {
        const data = await getVideoDetails({ admin, videoId: activeDetailId });
        setDetail(data);
        setDetailError("");
      } catch (err) {
        setDetailError(requestErrorMessage(err, "Could not refresh video details."));
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [activeDetailId, activeDetailStatus, admin]);

  return { detail, setDetail, detailError, loadDetail };
}

/** Admin portable-catalog addresses: load, republish, and copy-to-clipboard. */
export function useCatalogs(admin: boolean) {
  const [catalogs, setCatalogs] = useState<AdminCatalogs | null>(null);
  const [catalogPublishing, setCatalogPublishing] = useState(false);
  const [catalogCopied, setCatalogCopied] = useState("");
  const [catalogError, setCatalogError] = useState("");

  const loadCatalogs = useCallback(async () => {
    if (!admin) return;
    try {
      const data = await getAdminCatalogs();
      setCatalogs(data);
      setCatalogError("");
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Could not load catalog addresses."));
    }
  }, [admin]);

  useEffect(() => {
    loadCatalogs();
  }, [loadCatalogs]);

  const republishCatalogs = useCallback(async () => {
    setCatalogPublishing(true);
    setCatalogError("");
    setCatalogCopied("");
    try {
      const data = await publishAdminCatalogs();
      setCatalogs(data);
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Catalog publish failed."));
    } finally {
      setCatalogPublishing(false);
    }
  }, []);

  const copyAddress = useCallback(async (label: string, address?: string | null) => {
    if (!address) return;
    try {
      await navigator.clipboard.writeText(address);
      setCatalogCopied(label);
    } catch {
      setCatalogError("Could not copy the catalog address.");
    }
  }, []);

  return {
    catalogs,
    catalogPublishing,
    catalogCopied,
    catalogError,
    loadCatalogs,
    republishCatalogs,
    copyAddress,
  };
}

interface VideoActionDeps {
  load: () => Promise<void>;
  loadCatalogs: () => Promise<void>;
  onDeleted: (videoId: string) => void;
  setDetail: (detail: VideoDetail | null) => void;
  setVideos: React.Dispatch<React.SetStateAction<VideoSummary[]>>;
}

/** Admin mutations on a video: delete, approve, visibility, publication. */
export function useVideoActions({
  load,
  loadCatalogs,
  onDeleted,
  setDetail,
  setVideos,
}: VideoActionDeps) {
  const [approving, setApproving] = useState<string | null>(null);
  const [publishing, setPublishing] = useState<string | null>(null);
  const [actionError, setActionError] = useState("");

  const deleteVideo = useCallback(
    async (videoId: string) => {
      if (!window.confirm("Delete this video record and remove it from the network catalog?"))
        return;
      setActionError("");
      try {
        await deleteVideoRecord(videoId);
        setVideos((prev) => prev.filter((video) => video.id !== videoId));
        await loadCatalogs();
        onDeleted(videoId);
      } catch (err) {
        setActionError(requestErrorMessage(err, "Delete failed."));
      }
    },
    [loadCatalogs, onDeleted, setVideos],
  );

  const approveVideo = useCallback(
    async (videoId: string) => {
      setApproving(videoId);
      setActionError("");
      try {
        const data = await approveVideoUpload(videoId);
        setDetail(data);
        await load();
        await loadCatalogs();
      } catch (err) {
        const message = requestErrorMessage(err, "Approval failed.");
        setActionError(message);
        window.alert(message);
      } finally {
        setApproving(null);
      }
    },
    [load, loadCatalogs, setDetail],
  );

  const updateVisibility = useCallback(
    async (videoId: string, next: VisibilityUpdate) => {
      setActionError("");
      try {
        const data = await updateVideoVisibility(videoId, next);
        setDetail(data);
        setVideos((prev) =>
          prev.map((video) => (video.id === videoId ? { ...video, ...data } : video)),
        );
        await loadCatalogs();
      } catch (err) {
        setActionError(requestErrorMessage(err, "Visibility update failed."));
      }
    },
    [loadCatalogs, setDetail, setVideos],
  );

  const updatePublication = useCallback(
    async (videoId: string, isPublic: boolean) => {
      setPublishing(videoId);
      setActionError("");
      try {
        const data = await updateVideoPublication(videoId, isPublic);
        setDetail(data);
        setVideos((prev) =>
          prev.map((video) => (video.id === videoId ? { ...video, ...data } : video)),
        );
        await loadCatalogs();
      } catch (err) {
        setActionError(
          requestErrorMessage(err, isPublic ? "Publish failed." : "Unpublish failed."),
        );
      } finally {
        setPublishing(null);
      }
    },
    [loadCatalogs, setDetail, setVideos],
  );

  return {
    approving,
    publishing,
    actionError,
    setActionError,
    deleteVideo,
    approveVideo,
    updateVisibility,
    updatePublication,
  };
}
