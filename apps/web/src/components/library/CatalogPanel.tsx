import type { AdminCatalogs } from "../../types";

interface CatalogPanelProps {
  catalogCopied: string;
  catalogPublishing: boolean;
  catalogs: AdminCatalogs | null;
  onCopy: (label: string, address?: string | null) => void;
  onRepublish: () => void;
}

export default function CatalogPanel({
  catalogs,
  catalogPublishing,
  catalogCopied,
  onCopy,
  onRepublish,
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
          disabled={catalogPublishing}
          onClick={onRepublish}
        >
          {catalogPublishing ? "Publishing..." : "Republish"}
        </button>
      </div>
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
