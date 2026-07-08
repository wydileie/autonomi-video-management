import type { QuoteState } from "../../hooks/useUploadWorkflow";
import { formatAttoTokens, formatBytes, formatWei } from "../../utils/format";

export default function UploadQuoteSummary({ quote }: { quote: QuoteState }) {
  return (
    <div className="quote-panel">
      <div className="quote-main">
        <span className="meta-label">Upload price quote</span>
        {quote.loading && <strong>Quoting Autonomi storage...</strong>}
        {!quote.loading && quote.data && (
          <strong>{formatAttoTokens(quote.data.storage_cost_atto)}</strong>
        )}
        {!quote.loading && !quote.data && (
          <strong>{quote.error ? "Quote unavailable" : "Waiting for video duration"}</strong>
        )}
        <p>
          {quote.data
            ? `${formatBytes(quote.data.estimated_bytes)} across ${quote.data.segment_count} HLS segments${quote.data.original_file ? ", original file," : ""} and metadata`
            : quote.error || "The estimate refreshes when renditions change."}
        </p>
      </div>
      {quote.data && (
        <div className="quote-breakdown">
          <span>{formatWei(quote.data.estimated_gas_cost_wei)}</span>
          <span>{quote.data.payment_mode} payment mode</span>
          {quote.data.original_file && (
            <span>{formatBytes(quote.data.original_file.estimated_bytes)} original source</span>
          )}
          {quote.data.sampled && <span>large segment estimate sampled</span>}
        </div>
      )}
    </div>
  );
}
