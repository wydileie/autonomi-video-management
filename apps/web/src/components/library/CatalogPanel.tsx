import { formatAttoTokens, formatWei, formatDateTime } from "../../utils/format";
import type { AdminCatalogs } from "../../types";

interface CatalogPanelProps {
  catalogCopied: string;
  catalogPublishing: boolean;
  catalogs: AdminCatalogs | null;
  onCopy: (label: string, address?: string | null) => void;
  onRepublish: () => void;
  onApprove: () => void;
  onResume: () => void;
}

export default function CatalogPanel({
  catalogs,
  catalogPublishing,
  catalogCopied,
  onCopy,
  onRepublish,
  onApprove,
  onResume,
}: CatalogPanelProps) {
  return (
    <div className="catalog-address-panel">
      <div className="catalog-address-head">
        <div>
          <strong>Portable catalogs</strong>
          <span>
            Published {catalogs?.published_catalog?.videos.length ?? 0} / all{" "}
            {catalogs?.all_catalog?.videos.length ?? 0}
          </span>
        </div>
        <button
          type="button"
          className="secondary-action"
          disabled={
            catalogPublishing ||
            catalogs?.preparing ||
            ["approved", "uploading", "payment_recovery_required"].includes(
              catalogs?.publication?.state || "",
            )
          }
          onClick={onRepublish}
        >
          {catalogPublishing || catalogs?.preparing ? "Preparing..." : "Quote catalog publication"}
        </button>
      </div>
      {catalogs?.publication?.state === "draft" && (
        <div className="quote-panel">
          <strong>
            Storage cap: {formatAttoTokens(catalogs.publication.approval.max_storage_atto)}
          </strong>
          <span>Gas cap: {formatWei(catalogs.publication.approval.max_gas_wei)}</span>
          <p>
            Approval expires{" "}
            {formatDateTime(
              new Date(catalogs.publication.approval.expires_at * 1000).toISOString(),
            )}
            . Changes to the catalog require a new quote.
          </p>
          <button type="button" disabled={catalogPublishing} onClick={onApprove}>
            Approve catalog storage and gas caps
          </button>
        </div>
      )}
      {catalogs?.publication && ["approved", "uploading"].includes(catalogs.publication.state) && (
        <p>Publishing the approved catalog snapshot...</p>
      )}
      {catalogs?.publication?.state === "payment_recovery_required" && (
        <button type="button" disabled={catalogPublishing} onClick={onResume}>
          Resume approved catalog publication
        </button>
      )}
      {catalogs?.publication?.error && <p role="alert">{catalogs.publication.error}</p>}
      <div className="catalog-address-grid">
        <CatalogAddress
          label="Published"
          address={catalogs?.published_catalog_address}
          copied={catalogCopied === "published"}
          onCopy={() => onCopy("published", catalogs?.published_catalog_address)}
        />
        <CatalogAddress
          label="All"
          address={catalogs?.all_catalog_address}
          copied={catalogCopied === "all"}
          onCopy={() => onCopy("all", catalogs?.all_catalog_address)}
        />
      </div>
    </div>
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
