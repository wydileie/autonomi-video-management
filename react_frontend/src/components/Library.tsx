import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { useNavigate, useParams } from "react-router-dom";

import {
  approveVideoUpload,
  deleteVideoRecord,
  getVideoDetails,
  listVideos,
  requestErrorMessage,
  updateVideoPublication,
  updateVideoVisibility,
} from "../api/client";
import type { VideoDetail, VideoSummary, VisibilityUpdate } from "../types";
import { isActiveStatus, statusLabel } from "../utils/status";
import FinalQuotePanel from "./FinalQuotePanel";
import VideoPlayer from "./VideoPlayer";

interface LibraryProps {
  admin?: boolean;
  token?: string;
}

interface PlayingState {
  resolution: string;
  videoId: string;
}

export default function Library({ admin = false, token = "" }: LibraryProps) {
  const navigate = useNavigate();
  const { videoId: routeVideoId } = useParams<{ videoId?: string }>();
  const detailBasePath = admin ? "/manage" : "/library";
  const [videos, setVideos] = useState<VideoSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [playing, setPlaying] = useState<PlayingState | null>(null);
  const [detail, setDetail] = useState<VideoDetail | null>(null);
  const [approving, setApproving] = useState<string | null>(null);
  const [publishing, setPublishing] = useState<string | null>(null);
  const [loadError, setLoadError] = useState("");
  const [detailError, setDetailError] = useState("");
  const [actionError, setActionError] = useState("");
  const activeDetailId = detail?.id;
  const activeDetailStatus = detail?.status;

  const load = useCallback(async () => {
    try {
      const data = await listVideos({ admin, token });
      setVideos(data);
      setLoadError("");
    } catch (err) {
      setLoadError(requestErrorMessage(err, "Could not load the video catalog."));
    } finally {
      setLoading(false);
    }
  }, [admin, token]);

  const loadDetail = useCallback(async (videoId: string) => {
    setDetailError("");
    setActionError("");
    try {
      const data = await getVideoDetails({ admin, token, videoId });
      setDetail(data);
    } catch (err) {
      setDetailError(requestErrorMessage(err, "Could not load video details."));
    }
  }, [admin, token]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!routeVideoId) {
      setDetail(null);
      return;
    }

    let active = true;
    setDetailError("");
    setActionError("");
    getVideoDetails({ admin, token, videoId: routeVideoId })
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
  }, [admin, routeVideoId, token]);

  useEffect(() => {
    const interval = setInterval(() => {
      if (videos.some((video) => isActiveStatus(video.status))) {
        load();
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [videos, load]);

  useEffect(() => {
    if (!activeDetailId || !isActiveStatus(activeDetailStatus)) return undefined;
    const interval = setInterval(async () => {
      try {
        const data = await getVideoDetails({ admin, token, videoId: activeDetailId });
        setDetail(data);
        setDetailError("");
      } catch (err) {
        setDetailError(requestErrorMessage(err, "Could not refresh video details."));
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [activeDetailId, activeDetailStatus, admin, token]);

  const openDetail = async (videoId: string) => {
    if (routeVideoId === videoId && detail?.id === videoId) {
      navigate(detailBasePath);
      return;
    }
    if (routeVideoId === videoId) {
      await loadDetail(videoId);
      return;
    }
    navigate(`${detailBasePath}/${encodeURIComponent(videoId)}`);
  };

  const deleteVideo = async (videoId: string, event: MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    if (!window.confirm("Delete this video record and remove it from the network catalog?")) return;
    setActionError("");
    try {
      await deleteVideoRecord(token, videoId);
      setVideos((prev) => prev.filter((video) => video.id !== videoId));
      if (detail?.id === videoId) {
        setDetail(null);
        navigate(detailBasePath, { replace: true });
      }
      if (playing?.videoId === videoId) setPlaying(null);
    } catch (err) {
      setActionError(requestErrorMessage(err, "Delete failed."));
    }
  };

  const approveVideo = async (videoId: string) => {
    setApproving(videoId);
    setActionError("");
    try {
      const data = await approveVideoUpload(token, videoId);
      setDetail(data);
      await load();
    } catch (err) {
      const message = requestErrorMessage(err, "Approval failed.");
      setActionError(message);
      window.alert(message);
    } finally {
      setApproving(null);
    }
  };

  const updateVisibility = async (videoId: string, next: VisibilityUpdate) => {
    setActionError("");
    try {
      const data = await updateVideoVisibility(token, videoId, next);
      setDetail(data);
      setVideos((prev) => prev.map((video) => (video.id === videoId ? { ...video, ...data } : video)));
    } catch (err) {
      setActionError(requestErrorMessage(err, "Visibility update failed."));
    }
  };

  const updatePublication = async (videoId: string, isPublic: boolean) => {
    setPublishing(videoId);
    setActionError("");
    try {
      const data = await updateVideoPublication(token, videoId, isPublic);
      setDetail(data);
      setVideos((prev) => prev.map((video) => (video.id === videoId ? { ...video, ...data } : video)));
    } catch (err) {
      setActionError(requestErrorMessage(err, isPublic ? "Publish failed." : "Unpublish failed."));
    } finally {
      setPublishing(null);
    }
  };

  if (loading) {
    return (
      <div className="empty-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>Loading the catalog...</strong>
      </div>
    );
  }
  if (loadError && !videos.length) {
    return (
      <div className="empty-state error-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>{loadError}</strong>
      </div>
    );
  }
  if (!videos.length) {
    return (
      <div className="empty-state">
        <span className="empty-icon" aria-hidden="true" />
        <strong>
          {admin ? "No videos yet. Upload one to build your first stream." : "No videos are available yet."}
        </strong>
      </div>
    );
  }

  return (
    <section className="library-card">
      <div className="section-kicker">Catalog</div>
      <div className="library-head">
        <div>
          <h2>AutVid library</h2>
          <p>{admin ? "Manage processing, publishing, and public metadata." : "Browse published streams."}</p>
        </div>
        <span>{videos.length} video{videos.length === 1 ? "" : "s"}</span>
      </div>
      {loadError && <div className="error-box">{loadError}</div>}
      {detailError && <div className="error-box">{detailError}</div>}
      {actionError && <div className="error-box">{actionError}</div>}

      <div className="video-list">
        {videos.map((video) => (
          <article className="video-row" key={video.id}>
            <button type="button" className="video-summary" onClick={() => openDetail(video.id)}>
              <div className="video-title">
                <span className="video-thumb" aria-hidden="true">
                  <span />
                </span>
                <div>
                  <strong>{video.title}</strong>
                  <span>{video.original_filename || video.description || "Published stream"}</span>
                </div>
              </div>
              <div className="row-meta">
                {admin && (
                  <span className={`status ${video.is_public ? "public" : "private"}`}>
                    {video.is_public ? "public" : "hidden"}
                  </span>
                )}
                <span className={`status ${video.status}`}>{statusLabel(video.status)}</span>
                <span>{new Date(video.created_at).toLocaleDateString()}</span>
              </div>
            </button>

            {detail?.id === video.id && (
              <div className="video-detail">
                {admin && detail.status === "awaiting_approval" ? (
                  <FinalQuotePanel
                    quote={detail.final_quote}
                    expiresAt={detail.approval_expires_at}
                    approving={approving === video.id}
                    onApprove={() => approveVideo(video.id)}
                  />
                ) : admin && detail.status === "uploading" ? (
                  <p className="muted">Uploading approved segments and publishing the network manifest...</p>
                ) : admin && (detail.status === "processing" || detail.status === "pending") ? (
                  <p className="muted">Processing renditions and preparing the final quote...</p>
                ) : admin && (detail.status === "error" || detail.status === "expired") ? (
                  <p className="muted">{detail.error_message || "This video could not be completed."}</p>
                ) : detail.variants.length === 0 ? (
                  <p className="muted">No variants available.</p>
                ) : (
                  <>
                    {(() => {
                      const selectedVariant = detail.variants.find((variant) => (
                        playing?.videoId === video.id && playing?.resolution === variant.resolution
                      )) || detail.variants[0];

                      return (
                        <VideoPlayer
                          videoId={video.id}
                          manifestAddress={admin ? detail.manifest_address : null}
                          variants={detail.variants}
                          resolution={selectedVariant.resolution}
                          onResolutionChange={(nextResolution) => setPlaying({
                            videoId: video.id,
                            resolution: nextResolution,
                          })}
                        />
                      );
                    })()}
                  </>
                )}
                {admin && (
                  <>
                    {detail.status === "ready" && (
                      <div className="publication-panel">
                        <button
                          type="button"
                          className={detail.is_public ? "secondary-action" : "primary-action compact-action"}
                          disabled={publishing === video.id}
                          onClick={() => updatePublication(video.id, !detail.is_public)}
                        >
                          {publishing === video.id
                            ? "Updating..."
                            : detail.is_public ? "Unpublish" : "Publish"}
                        </button>
                        <span className={`status ${detail.is_public ? "public" : "private"}`}>
                          {detail.is_public ? "public" : "hidden"}
                        </span>
                      </div>
                    )}
                    <div className="visibility-panel">
                      <label>
                        <input
                          type="checkbox"
                          checked={!!detail.show_manifest_address}
                          onChange={(event) => updateVisibility(video.id, {
                            show_original_filename: false,
                            show_manifest_address: event.target.checked,
                          })}
                        />
                        <span>Publish manifest address</span>
                      </label>
                    </div>
                  </>
                )}
                {(admin || detail.manifest_address) && (
                  <div className="detail-footer">
                    <div className="detail-addresses">
                      <code>{detail.manifest_address || "Manifest hidden or pending"}</code>
                      {admin && detail.original_file_address && (
                        <code>Original source: {detail.original_file_address}</code>
                      )}
                    </div>
                    {admin && (
                      <button type="button" className="danger-action" onClick={(event) => deleteVideo(video.id, event)}>
                        Delete
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
