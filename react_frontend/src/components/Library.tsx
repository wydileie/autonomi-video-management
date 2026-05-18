import { useVideoLibrary } from "../hooks/useVideoLibrary";
import { statusLabel } from "../utils/status";
import FinalQuotePanel from "./FinalQuotePanel";
import VideoPlayer from "./VideoPlayer";

interface LibraryProps {
  admin?: boolean;
}

export default function Library({ admin = false }: LibraryProps) {
  const {
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
  } = useVideoLibrary(admin);

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
      {catalogError && <div className="error-box">{catalogError}</div>}

      {admin && (
        <div className="catalog-address-panel">
          <div className="catalog-address-head">
            <div>
              <strong>Portable catalogs</strong>
              <span>
                Published {catalogs?.published_catalog?.videos.length ?? 0} / all {catalogs?.all_catalog?.videos.length ?? 0}
              </span>
            </div>
            <button
              type="button"
              className="secondary-action"
              disabled={catalogPublishing}
              onClick={republishCatalogs}
            >
              {catalogPublishing ? "Publishing..." : "Republish"}
            </button>
          </div>
          <div className="catalog-address-grid">
            <CatalogAddress
              label="Published"
              address={catalogs?.published_catalog_address}
              copied={catalogCopied === "published"}
              onCopy={() => copyAddress("published", catalogs?.published_catalog_address)}
            />
            <CatalogAddress
              label="All"
              address={catalogs?.all_catalog_address}
              copied={catalogCopied === "all"}
              onCopy={() => copyAddress("all", catalogs?.all_catalog_address)}
            />
          </div>
        </div>
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

function CatalogAddress({
  address,
  copied,
  label,
  onCopy,
}: {
  address?: string | null;
  copied: boolean;
  label: string;
  onCopy: () => void;
}) {
  return (
    <div className="catalog-address-row">
      <span>{label}</span>
      <code>{address || "Not published yet"}</code>
      <button type="button" className="secondary-action" disabled={!address} onClick={onCopy}>
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}
