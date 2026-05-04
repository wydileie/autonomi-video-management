import { formatAttoTokens, formatBytes, formatDateTime, formatWei } from "../utils/format";

export default function FinalQuotePanel({ quote, expiresAt, onApprove, approving }) {
  if (!quote) {
    return <p className="muted">Preparing the final quote from transcoded segments...</p>;
  }
  const originalBytes = quote.original_file?.byte_size || quote.original_file?.estimated_bytes || 0;
  const transcodedBytes = quote.actual_transcoded_bytes || quote.actual_media_bytes || quote.estimated_bytes;

  return (
    <div className="quote-panel final-quote-panel">
      <div className="quote-main">
        <span className="meta-label">Final Autonomi quote</span>
        <strong>{formatAttoTokens(quote.storage_cost_atto)}</strong>
        <p>
          {originalBytes
            ? `${formatBytes(transcodedBytes)} transcoded media plus ${formatBytes(originalBytes)} original source`
            : `${formatBytes(transcodedBytes)} of transcoded media`}
          across {quote.segment_count} HLS segments. Approval expires {formatDateTime(expiresAt || quote.approval_expires_at)}.
        </p>
      </div>
      <div className="quote-breakdown">
        <span>{formatWei(quote.estimated_gas_cost_wei)}</span>
        <span>{formatBytes(quote.metadata_bytes)} metadata estimate</span>
        {originalBytes > 0 && <span>{formatBytes(originalBytes)} original source</span>}
        <span>{quote.payment_mode} payment mode</span>
      </div>
      <button type="button" className="approve-action" onClick={onApprove} disabled={approving}>
        {approving ? "Approving..." : "Approve upload"}
      </button>
    </div>
  );
}
