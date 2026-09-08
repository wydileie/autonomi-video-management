import { formatAttoTokens, formatBytes, formatDateTime, formatWei } from "../utils/format";
import type { UploadQuote } from "../types";

interface FinalQuotePanelProps {
  approving: boolean;
  expiresAt?: string | null;
  onApprove: () => void;
  quote?: UploadQuote | null;
}

export default function FinalQuotePanel({
  quote,
  expiresAt,
  onApprove,
  approving,
}: FinalQuotePanelProps) {
  if (!quote) {
    return <p className="muted">Preparing the final quote from transcoded segments...</p>;
  }
  const originalBytes = quote.original_file?.byte_size || quote.original_file?.estimated_bytes || 0;
  const transcodedBytes =
    quote.actual_transcoded_bytes || quote.actual_media_bytes || quote.estimated_bytes;

  return (
    <div className="quote-panel final-quote-panel">
      <div className="quote-main">
        <span className="meta-label">Approved spending limits</span>
        <strong>Storage cap: {formatAttoTokens(quote.storage_cost_atto)}</strong>
        <p>
          {originalBytes
            ? `${formatBytes(transcodedBytes)} transcoded media plus ${formatBytes(originalBytes)} original source`
            : `${formatBytes(transcodedBytes)} of transcoded media`}
          across {quote.segment_count} HLS segments. Approval expires{" "}
          {formatDateTime(expiresAt || quote.approval_expires_at)}.
        </p>
      </div>
      <div className="quote-breakdown">
        <span>Gas cap: {formatWei(quote.estimated_gas_cost_wei)}</span>
        <span>{formatBytes(quote.metadata_bytes || 0)} manifest and catalogs</span>
        {originalBytes > 0 && <span>{formatBytes(originalBytes)} original source</span>}
        <span>{quote.payment_mode} payment mode</span>
      </div>
      <p className="muted">
        Payments stop if content changes or either cap is exhausted. Network:{" "}
        {quote.approval?.network || "Quote must be regenerated"}.
      </p>
      <button
        type="button"
        className="approve-action"
        onClick={onApprove}
        disabled={approving || !quote.approval}
      >
        {approving ? "Approving..." : "Approve storage and gas caps"}
      </button>
    </div>
  );
}
