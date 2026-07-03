import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { useNavigate, useParams } from "react-router-dom";

import { useCatalogs, useLibraryData, useVideoActions, useVideoDetail } from "../hooks/useLibrary";
import { statusLabel } from "../utils/status";
import CatalogPanel from "./library/CatalogPanel";
import VideoDetailPane from "./library/VideoDetailPane";

interface LibraryProps {
  admin?: boolean;
}

interface PlayingState {
  resolution: string;
  videoId: string;
}

export default function Library({ admin = false }: LibraryProps) {
  const navigate = useNavigate();
  const { videoId: routeVideoId } = useParams<{ videoId?: string }>();
  const detailBasePath = admin ? "/manage" : "/library";
  const [playing, setPlaying] = useState<PlayingState | null>(null);

  const { videos, setVideos, loading, loadError, load } = useLibraryData(admin);
  const { detail, setDetail, detailError, loadDetail } = useVideoDetail(admin, routeVideoId);
  const {
    catalogs,
    catalogPublishing,
    catalogCopied,
    catalogError,
    loadCatalogs,
    republishCatalogs,
    copyAddress,
  } = useCatalogs(admin);

  const onDeleted = useCallback(
    (videoId: string) => {
      if (detail?.id === videoId) {
        setDetail(null);
        navigate(detailBasePath, { replace: true });
      }
      setPlaying((prev) => (prev?.videoId === videoId ? null : prev));
    },
    [detail?.id, detailBasePath, navigate, setDetail],
  );

  const {
    approving,
    publishing,
    actionError,
    setActionError,
    deleteVideo,
    approveVideo,
    updateVisibility,
    updatePublication,
  } = useVideoActions({ load, loadCatalogs, onDeleted, setDetail, setVideos });

  useEffect(() => {
    setActionError("");
  }, [routeVideoId, setActionError]);

  const openDetail = async (videoId: string) => {
    if (routeVideoId === videoId && detail?.id === videoId) {
      navigate(detailBasePath);
      return;
    }
    if (routeVideoId === videoId) {
      setActionError("");
      await loadDetail(videoId);
      return;
    }
    navigate(`${detailBasePath}/${encodeURIComponent(videoId)}`);
  };

  const handleDelete = (videoId: string) => (event: MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    deleteVideo(videoId);
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
          {admin
            ? "No videos yet. Upload one to build your first stream."
            : "No videos are available yet."}
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
          <p>
            {admin
              ? "Manage processing, publishing, and public metadata."
              : "Browse published streams."}
          </p>
        </div>
        <span>
          {videos.length} video{videos.length === 1 ? "" : "s"}
        </span>
      </div>
      {loadError && <div className="error-box">{loadError}</div>}
      {detailError && <div className="error-box">{detailError}</div>}
      {actionError && <div className="error-box">{actionError}</div>}
      {catalogError && <div className="error-box">{catalogError}</div>}

      {admin && (
        <CatalogPanel
          catalogs={catalogs}
          catalogPublishing={catalogPublishing}
          catalogCopied={catalogCopied}
          onCopy={copyAddress}
          onRepublish={republishCatalogs}
        />
      )}

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
              <VideoDetailPane
                admin={admin}
                detail={detail}
                approving={approving === video.id}
                publishing={publishing === video.id}
                selectedResolution={playing?.videoId === video.id ? playing.resolution : null}
                onApprove={() => approveVideo(video.id)}
                onDelete={handleDelete(video.id)}
                onResolutionChange={(nextResolution) =>
                  setPlaying({ videoId: video.id, resolution: nextResolution })
                }
                onUpdatePublication={(isPublic) => updatePublication(video.id, isPublic)}
                onUpdateVisibility={(next) => updateVisibility(video.id, next)}
              />
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
