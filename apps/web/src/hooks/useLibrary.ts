import { useCallback, useEffect, useRef, useState } from "react";

import {
  approveVideoUpload,
  approveAdminCatalogs,
  resumeAdminCatalogs,
  requoteVideoUpload,
  resumeVideoUpload,
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

  const pending = useRef<AbortController | null>(null);
  const load = useCallback(async () => {
    pending.current?.abort();
    const request = new AbortController();
    pending.current = request;
    try {
      const data = await listVideos({ admin, signal: request.signal });
      if (request.signal.aborted) return;
      setVideos(data);
      setLoadError("");
    } catch (err) {
      if (!request.signal.aborted)
        setLoadError(requestErrorMessage(err, "Could not load the video catalog."));
    } finally {
      if (!request.signal.aborted) setLoading(false);
      if (pending.current === request) pending.current = null;
    }
  }, [admin]);

  useEffect(() => {
    setVideos([]);
    setLoading(true);
    void load();
    return () => pending.current?.abort();
  }, [load]);

  useEffect(() => {
    if (!videos.some((video) => isActiveStatus(video.status))) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!pending.current) await load();
      if (active) timer = setTimeout(poll, 5000);
    };
    timer = setTimeout(poll, 5000);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [videos, load]);

  return { videos, setVideos, loading, loadError, load };
}

/** Route-driven video detail with 5s polling while the video is active. */
export function useVideoDetail(admin: boolean, routeVideoId: string | undefined) {
  const [detail, setDetail] = useState<VideoDetail | null>(null);
  const [detailError, setDetailError] = useState("");
  const pending = useRef<AbortController | null>(null);
  const selection = useRef({ admin, routeVideoId });
  useEffect(() => {
    selection.current = { admin, routeVideoId };
  }, [admin, routeVideoId]);
  const loadDetail = useCallback(
    async (videoId: string) => {
      if (videoId !== selection.current.routeVideoId || admin !== selection.current.admin) return;
      pending.current?.abort();
      const request = new AbortController();
      pending.current = request;
      try {
        const data = await getVideoDetails({ admin, videoId, signal: request.signal });
        if (
          request.signal.aborted ||
          videoId !== selection.current.routeVideoId ||
          admin !== selection.current.admin
        )
          return;
        setDetail(data);
        setDetailError("");
      } catch (err) {
        if (!request.signal.aborted)
          setDetailError(requestErrorMessage(err, "Could not load video details."));
      } finally {
        if (pending.current === request) pending.current = null;
      }
    },
    [admin],
  );

  useEffect(() => {
    setDetail(null);
    setDetailError("");
    if (routeVideoId) void loadDetail(routeVideoId);
    return () => pending.current?.abort();
  }, [loadDetail, routeVideoId]);

  useEffect(() => {
    if (!detail || detail.id !== routeVideoId || !isActiveStatus(detail.status)) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!pending.current) await loadDetail(detail.id);
      if (active) timer = setTimeout(poll, 5000);
    };
    timer = setTimeout(poll, 5000);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [detail, routeVideoId, loadDetail]);

  const setSelectedDetail = useCallback(
    (next: VideoDetail | null) => {
      if (
        !next ||
        (selection.current.admin === admin && selection.current.routeVideoId === next.id)
      )
        setDetail(next);
    },
    [admin],
  );
  return { detail, setDetail: setSelectedDetail, detailError, loadDetail };
}

/** Admin portable-catalog addresses: load, republish, and copy-to-clipboard. */
export function useCatalogs(admin: boolean) {
  const [catalogs, setCatalogs] = useState<AdminCatalogs | null>(null);
  const [catalogPublishing, setCatalogPublishing] = useState(false);
  const [catalogCopied, setCatalogCopied] = useState("");
  const [catalogError, setCatalogError] = useState("");

  const pending = useRef<AbortController | null>(null);
  const loadCatalogs = useCallback(async () => {
    if (!admin) return;
    pending.current?.abort();
    const request = new AbortController();
    pending.current = request;
    try {
      const data = await getAdminCatalogs(request.signal);
      if (!request.signal.aborted) {
        setCatalogs(data);
        setCatalogError("");
      }
    } catch (err) {
      if (!request.signal.aborted)
        setCatalogError(requestErrorMessage(err, "Could not load catalog addresses."));
    } finally {
      if (pending.current === request) pending.current = null;
    }
  }, [admin]);

  useEffect(() => {
    setCatalogs(null);
    if (!admin) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!pending.current) await loadCatalogs();
      if (active) timer = setTimeout(poll, 5000);
    };
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
      pending.current?.abort();
    };
  }, [admin, loadCatalogs]);

  const republishCatalogs = useCallback(async () => {
    setCatalogPublishing(true);
    setCatalogError("");
    setCatalogCopied("");
    try {
      await publishAdminCatalogs();
      await loadCatalogs();
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Catalog quote failed."));
    } finally {
      setCatalogPublishing(false);
    }
  }, [loadCatalogs]);

  const approveCatalogs = useCallback(async () => {
    if (catalogs?.publication?.state !== "draft") return;
    setCatalogPublishing(true);
    setCatalogError("");
    try {
      await approveAdminCatalogs(catalogs.publication.approval);
      await loadCatalogs();
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Catalog approval failed."));
    } finally {
      setCatalogPublishing(false);
    }
  }, [catalogs, loadCatalogs]);

  const resumeCatalogs = useCallback(async () => {
    setCatalogPublishing(true);
    setCatalogError("");
    try {
      await resumeAdminCatalogs();
      await loadCatalogs();
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Catalog recovery remains paused."));
    } finally {
      setCatalogPublishing(false);
    }
  }, [loadCatalogs]);

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
    approveCatalogs,
    resumeCatalogs,
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
    async (
      videoId: string,
      approval: { quote_id: string; max_storage_atto: string; max_gas_wei: string },
    ) => {
      setApproving(videoId);
      setActionError("");
      try {
        const data = await approveVideoUpload(videoId, approval);
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

  const requoteVideo = useCallback(
    async (videoId: string) => {
      setActionError("");
      try {
        setDetail(await requoteVideoUpload(videoId));
        await load();
      } catch (err) {
        setActionError(requestErrorMessage(err, "Could not regenerate quote."));
      }
    },
    [load, setDetail],
  );

  const resumeVideo = useCallback(
    async (videoId: string) => {
      setActionError("");
      try {
        setDetail(await resumeVideoUpload(videoId));
        await load();
      } catch (err) {
        setActionError(requestErrorMessage(err, "Could not resume the approved upload."));
      }
    },
    [load, setDetail],
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
    requoteVideo,
    resumeVideo,
    updateVisibility,
    updatePublication,
  };
}
