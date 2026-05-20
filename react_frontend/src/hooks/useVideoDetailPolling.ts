import { useCallback, useEffect, useState } from "react";

import { getVideoDetails, requestErrorMessage } from "../api/client";
import type { VideoDetail } from "../types";
import { isActiveStatus } from "../utils/status";

interface UseVideoDetailPollingOptions {
  admin: boolean;
  routeVideoId?: string;
}

export function useVideoDetailPolling({
  admin,
  routeVideoId,
}: UseVideoDetailPollingOptions) {
  const [detail, setDetail] = useState<VideoDetail | null>(null);
  const [detailError, setDetailError] = useState("");
  const [actionError, setActionError] = useState("");
  const activeDetailId = detail?.id;
  const activeDetailStatus = detail?.status;

  const loadDetail = useCallback(async (videoId: string) => {
    setDetailError("");
    setActionError("");
    try {
      const data = await getVideoDetails({ admin, videoId });
      setDetail(data);
    } catch (err) {
      setDetailError(requestErrorMessage(err, "Could not load video details."));
    }
  }, [admin]);

  const clearActionError = useCallback(() => {
    setActionError("");
  }, []);

  const showActionError = useCallback((message: string) => {
    setActionError(message);
  }, []);

  const replaceDetail = useCallback((nextDetail: VideoDetail) => {
    setDetail(nextDetail);
  }, []);

  const clearDetail = useCallback(() => {
    setDetail(null);
  }, []);

  useEffect(() => {
    if (!routeVideoId) {
      setDetail(null);
      return;
    }

    let active = true;
    setDetailError("");
    setActionError("");
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

  return {
    actionError,
    detail,
    detailError,
    clearActionError,
    clearDetail,
    loadDetail,
    replaceDetail,
    showActionError,
  };
}
