import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type DragEvent,
  type FormEvent,
} from "react";

import { isRequestCanceled, requestErrorMessage, requestUploadQuote, uploadVideo } from "../api/client";
import { RESOLUTION_OPTIONS } from "../constants";
import type { SourceVideoMeta, UploadQuote, UploadQuoteRequest, VideoSummary } from "../types";
import { formatAttoTokens, formatBytes, formatWei } from "../utils/format";
import {
  classifyResolution,
  optionFitsSource,
  orderedSelection,
  resolutionByValue,
  suggestedSelection,
  targetDimensionsForMeta,
} from "../utils/resolutions";

interface QuoteState {
  data: UploadQuote | null;
  error: string;
  loading: boolean;
}

interface UploadPanelProps {
  onUploaded: (video: VideoSummary) => void;
}

export default function UploadPanel({ onUploaded }: UploadPanelProps) {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [desc, setDesc] = useState("");
  const [showManifestAddress, setShowManifestAddress] = useState(false);
  const [uploadOriginal, setUploadOriginal] = useState(false);
  const [publishWhenReady, setPublishWhenReady] = useState(false);
  const [selected, setSelected] = useState<string[]>(["720p"]);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [meta, setMeta] = useState<SourceVideoMeta | null>(null);
  const [quote, setQuote] = useState<QuoteState>({ loading: false, error: "", data: null });

  const currentProfile = classifyResolution(meta?.width, meta?.height);

  const inspectFile = useCallback((nextFile?: File) => {
    if (!nextFile) return;
    if (!nextFile.type.startsWith("video/")) {
      setError("Please choose a video file.");
      return;
    }

    setError("");
    setFile(nextFile);
    setQuote({ loading: false, error: "", data: null });
    setTitle((current) => current || nextFile.name.replace(/\.[^.]+$/, ""));
    setMeta({ loading: true, width: null, height: null, duration: null });

    const objectUrl = URL.createObjectURL(nextFile);
    const video = document.createElement("video");
    video.preload = "metadata";
    video.onloadedmetadata = () => {
      const nextMeta = {
        loading: false,
        width: video.videoWidth,
        height: video.videoHeight,
        duration: video.duration,
        size: nextFile.size,
      };
      setMeta(nextMeta);
      setSelected(suggestedSelection(nextMeta));
      URL.revokeObjectURL(objectUrl);
    };
    video.onerror = () => {
      setMeta({ loading: false, width: null, height: null, duration: null, size: nextFile.size });
      setSelected(["720p"]);
      URL.revokeObjectURL(objectUrl);
    };
    video.src = objectUrl;
  }, []);

  useEffect(() => {
    const duration = meta?.duration;
    if (!file || !duration || !selected.length) {
      setQuote({ loading: false, error: "", data: null });
      return undefined;
    }

    const controller = new AbortController();
    const timer = setTimeout(async () => {
      const resolutions = orderedSelection(selected);
      setQuote({ loading: true, error: "", data: null });
      try {
        const quoteRequest: UploadQuoteRequest = {
          duration_seconds: duration,
          resolutions,
          source_width: meta.width,
          source_height: meta.height,
        };
        if (uploadOriginal) {
          quoteRequest.upload_original = true;
          quoteRequest.source_size_bytes = file.size;
        }
        const data = await requestUploadQuote(quoteRequest, controller.signal);
        setQuote({ loading: false, error: "", data });
      } catch (err) {
        if (isRequestCanceled(err)) return;
        setQuote({
          loading: false,
          error: requestErrorMessage(err, "Could not get upload price quote"),
          data: null,
        });
      }
    }, 250);

    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [file, meta?.duration, meta?.width, meta?.height, selected, uploadOriginal]);

  const onDrop = (event: DragEvent<HTMLButtonElement>) => {
    event.preventDefault();
    setDragging(false);
    inspectFile(event.dataTransfer.files?.[0]);
  };

  const toggleRes = (resolution: string) => {
    setSelected((prev) => (
      prev.includes(resolution)
        ? prev.filter((value) => value !== resolution)
        : [...prev, resolution]
    ));
  };

  const selectCurrentOnly = () => {
    if (currentProfile) setSelected([currentProfile.value]);
  };

  const selectAdaptive = () => {
    setSelected(suggestedSelection(meta));
  };

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!file) return setError("Drop or choose a video file first.");
    if (!title.trim()) return setError("Please enter a title.");
    if (!selected.length) return setError("Select at least one resolution.");
    if (meta?.duration && !quote.data) {
      return setError("Waiting for an upload price quote before starting.");
    }

    setError("");
    setUploading(true);
    setProgress(0);

    const resolutionsToUpload = orderedSelection(selected);

    const fd = new FormData();
    fd.append("file", file);
    fd.append("title", title.trim());
    fd.append("description", desc.trim());
    fd.append("resolutions", resolutionsToUpload.join(","));
    fd.append("show_original_filename", "false");
    fd.append("show_manifest_address", showManifestAddress ? "true" : "false");
    fd.append("upload_original", uploadOriginal ? "true" : "false");
    fd.append("publish_when_ready", publishWhenReady ? "true" : "false");

    try {
      const data = await uploadVideo(fd, (progressEvent) => {
        if (progressEvent.total) {
          setProgress(Math.round((progressEvent.loaded / progressEvent.total) * 100));
        }
      });
      setFile(null);
      setTitle("");
      setDesc("");
      setShowManifestAddress(false);
      setUploadOriginal(false);
      setPublishWhenReady(false);
      setSelected(["720p"]);
      setMeta(null);
      setQuote({ loading: false, error: "", data: null });
      setProgress(0);
      if (fileInputRef.current) fileInputRef.current.value = "";
      onUploaded(data);
    } catch (err) {
      setError(requestErrorMessage(err, "Upload failed"));
    } finally {
      setUploading(false);
    }
    return undefined;
  };

  return (
    <section className="upload-card">
      <div className="section-kicker">Ingest</div>
      <div className="upload-head">
        <div>
          <h1>Drop a video. Build a streaming ladder. Store it on Autonomi.</h1>
          <p>
            The browser reads the source dimensions locally, then we prepare the current
            resolution plus any lower renditions you choose.
          </p>
        </div>
        <div className="network-pill">Local devnet ready</div>
      </div>

      <form onSubmit={submit}>
        <button
          type="button"
          className={`dropzone ${dragging ? "is-dragging" : ""}`}
          onClick={() => fileInputRef.current?.click()}
          onDragEnter={(event) => {
            event.preventDefault();
            setDragging(true);
          }}
          onDragOver={(event) => event.preventDefault()}
          onDragLeave={() => setDragging(false)}
          onDrop={onDrop}
          disabled={uploading}
        >
          <input
            ref={fileInputRef}
            className="hidden-input"
            type="file"
            accept="video/*"
            onChange={(event) => inspectFile(event.target.files?.[0])}
            disabled={uploading}
          />
          <span className="drop-icon">+</span>
          <span className="drop-title">
            {file ? file.name : "Drag and drop a video file"}
          </span>
          <span className="drop-subtitle">
            {file ? `${formatBytes(file.size)} selected` : "or click to browse from your machine"}
          </span>
        </button>

        {file && (
          <div className="source-panel">
            <div>
              <span className="meta-label">Detected source</span>
              <strong>
                {meta?.loading
                  ? "Reading metadata..."
                  : meta?.width
                    ? `${meta.width} x ${meta.height}`
                    : "Resolution unavailable"}
              </strong>
            </div>
            <div>
              <span className="meta-label">Current profile</span>
              <strong>{currentProfile?.label || "Unknown"}</strong>
            </div>
            <div>
              <span className="meta-label">Duration</span>
              <strong>{meta?.duration ? `${Math.round(meta.duration)}s` : "Unknown"}</strong>
            </div>
          </div>
        )}

        <div className="form-grid">
          <label>
            <span>Title</span>
            <input value={title} onChange={(event) => setTitle(event.target.value)} disabled={uploading} />
          </label>
          <label>
            <span>Description</span>
            <input value={desc} onChange={(event) => setDesc(event.target.value)} disabled={uploading} />
          </label>
        </div>

        <div className="privacy-panel">
          <label>
            <input
              type="checkbox"
              checked={showManifestAddress}
              onChange={(event) => setShowManifestAddress(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish manifest address</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={uploadOriginal}
              onChange={(event) => {
                setUploadOriginal(event.target.checked);
                setQuote({ loading: false, error: "", data: null });
              }}
              disabled={uploading}
            />
            <span>Upload original source file</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={publishWhenReady}
              onChange={(event) => setPublishWhenReady(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish automatically when ready</span>
          </label>
        </div>

        <div className="resolution-toolbar">
          <div>
            <span className="meta-label">Renditions to create</span>
            <p>Higher-than-source options are dimmed to avoid accidental upscales.</p>
          </div>
          <div className="quick-actions">
            <button type="button" onClick={selectCurrentOnly} disabled={!currentProfile || uploading}>Current only</button>
            <button type="button" onClick={selectAdaptive} disabled={!file || uploading}>Current + lower</button>
          </div>
        </div>

        <div className="resolution-grid">
          {RESOLUTION_OPTIONS.map((option) => {
            const isCurrent = currentProfile?.value === option.value;
            const disabledBySource = !!file && !optionFitsSource(option, meta);
            const targetDimensions = targetDimensionsForMeta(option, meta);
            return (
              <button
                key={option.value}
                type="button"
                className={`resolution-card ${selected.includes(option.value) ? "selected" : ""}`}
                onClick={() => !disabledBySource && toggleRes(option.value)}
                disabled={uploading || disabledBySource}
              >
                <span className="resolution-label">{option.label}</span>
                <span>{targetDimensions.width} x {targetDimensions.height}</span>
                <span>{option.bitrate} · {option.note}</span>
                {isCurrent && <strong>Current source profile</strong>}
              </button>
            );
          })}
        </div>

        {file && (
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
                {quote.data.original_file && <span>{formatBytes(quote.data.original_file.estimated_bytes)} original source</span>}
                {quote.data.sampled && <span>large segment estimate sampled</span>}
              </div>
            )}
          </div>
        )}

        {uploading && (
          <div className="upload-progress">
            <div>
              <span>{progress < 100 ? `Uploading source file ${progress}%` : "Transcoding and preparing final quote..."}</span>
              <span>{selected.map((value) => resolutionByValue(value)?.label || value).join(", ")}</span>
            </div>
            <div className="progress-track"><div style={{ width: `${progress}%` }} /></div>
          </div>
        )}

        {error && <div className="error-box">{error}</div>}

        <button className="primary-action" type="submit" disabled={uploading || quote.loading}>
          {uploading ? "Creating final quote..." : "Upload source"}
        </button>
      </form>
    </section>
  );
}
