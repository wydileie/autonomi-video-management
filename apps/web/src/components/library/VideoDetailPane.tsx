import type { MouseEvent } from "react";

import type { VideoDetail, VisibilityUpdate } from "../../types";
import FinalQuotePanel from "../FinalQuotePanel";
import VideoPlayer from "../VideoPlayer";

interface VideoDetailPaneProps {
  admin: boolean;
  approving: boolean;
  detail: VideoDetail;
  onApprove: () => void;
  onDelete: (event: MouseEvent<HTMLButtonElement>) => void;
  onResolutionChange: (resolution: string) => void;
  onUpdatePublication: (isPublic: boolean) => void;
  onUpdateVisibility: (next: VisibilityUpdate) => void;
  publishing: boolean;
  selectedResolution: string | null;
}

export default function VideoDetailPane({
  admin,
  detail,
  approving,
  publishing,
  selectedResolution,
  onApprove,
  onDelete,
  onResolutionChange,
  onUpdatePublication,
  onUpdateVisibility,
}: VideoDetailPaneProps) {
  const selectedVariant =
    detail.variants.find((variant) => variant.resolution === selectedResolution) ||
    detail.variants[0];

  return (
    <div className="video-detail">
      {admin && detail.status === "awaiting_approval" ? (
        <FinalQuotePanel
          quote={detail.final_quote}
          expiresAt={detail.approval_expires_at}
          approving={approving}
          onApprove={onApprove}
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
        <VideoPlayer
          videoId={detail.id}
          manifestAddress={admin ? detail.manifest_address : null}
          variants={detail.variants}
          resolution={selectedVariant.resolution}
          onResolutionChange={onResolutionChange}
        />
      )}
      {admin && (
        <>
          {detail.status === "ready" && (
            <div className="publication-panel">
              <button
                type="button"
                className={detail.is_public ? "secondary-action" : "primary-action compact-action"}
                disabled={publishing}
                onClick={() => onUpdatePublication(!detail.is_public)}
              >
                {publishing ? "Updating..." : detail.is_public ? "Unpublish" : "Publish"}
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
                onChange={(event) =>
                  onUpdateVisibility({
                    show_original_filename: false,
                    show_manifest_address: event.target.checked,
                  })
                }
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
            <button type="button" className="danger-action" onClick={onDelete}>
              Delete
            </button>
          )}
        </div>
      )}
    </div>
  );
}
