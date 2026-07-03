import { useCallback, useRef, type FormEvent } from "react";

import {
  useUploadForm,
  useUploadQuote,
  useUploadSubmit,
  type VideoCodec,
} from "../hooks/useUploadWorkflow";
import type { VideoSummary } from "../types";
import { resolutionByValue } from "../utils/resolutions";
import EncodeSettingsFields from "./upload/EncodeSettingsFields";
import FileDropZone from "./upload/FileDropZone";
import UploadQuoteSummary from "./upload/UploadQuoteSummary";

interface UploadPanelProps {
  onUploaded: (video: VideoSummary) => void;
}

export default function UploadPanel({ onUploaded }: UploadPanelProps) {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const form = useUploadForm();
  const { quote, resetQuote } = useUploadQuote(form);

  const handleUploadSuccess = useCallback(
    (video: VideoSummary) => {
      form.reset();
      resetQuote();
      if (fileInputRef.current) fileInputRef.current.value = "";
      onUploaded(video);
    },
    [form, onUploaded, resetQuote],
  );

  const { uploading, error, setError, progress, submit } = useUploadSubmit({
    form,
    quote,
    onSuccess: handleUploadSuccess,
  });

  const handleFile = (nextFile?: File) => {
    if (!nextFile) return;
    if (!nextFile.type.startsWith("video/")) {
      setError("Please choose a video file.");
      return;
    }
    setError("");
    resetQuote();
    form.inspectFile(nextFile);
  };

  const handleCodecChange = (codec: VideoCodec) => {
    form.changeCodec(codec);
    resetQuote();
  };

  const handleAudioBitrateChange = (kbps: number) => {
    form.setAudioBitrateKbps(kbps);
    resetQuote();
  };

  const handleVideoBitrateChange = (resolution: string, kbps: number) => {
    form.setVideoBitrate(resolution, kbps);
    resetQuote();
  };

  const handleUploadOriginalChange = (checked: boolean) => {
    form.setUploadOriginal(checked);
    resetQuote();
  };

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    submit();
  };

  const { file, meta, currentProfile, selected } = form;

  return (
    <section className="upload-card">
      <div className="section-kicker">Ingest</div>
      <div className="upload-head">
        <div>
          <h1>Drop a video. Build a streaming ladder. Store it on Autonomi.</h1>
          <p>
            The browser reads the source dimensions locally, then we prepare the current resolution
            plus any lower renditions you choose.
          </p>
        </div>
        <div className="network-pill">Local devnet ready</div>
      </div>

      <form onSubmit={onSubmit}>
        <FileDropZone
          file={file}
          fileInputRef={fileInputRef}
          onFile={handleFile}
          uploading={uploading}
        />

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
            <input
              value={form.title}
              onChange={(event) => form.setTitle(event.target.value)}
              disabled={uploading}
            />
          </label>
          <label>
            <span>Description</span>
            <input
              value={form.desc}
              onChange={(event) => form.setDesc(event.target.value)}
              disabled={uploading}
            />
          </label>
        </div>

        <div className="privacy-panel">
          <label>
            <input
              type="checkbox"
              checked={form.showManifestAddress}
              onChange={(event) => form.setShowManifestAddress(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish manifest address</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={form.uploadOriginal}
              onChange={(event) => handleUploadOriginalChange(event.target.checked)}
              disabled={uploading}
            />
            <span>Upload original source file</span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={form.publishWhenReady}
              onChange={(event) => form.setPublishWhenReady(event.target.checked)}
              disabled={uploading}
            />
            <span>Publish automatically when ready</span>
          </label>
        </div>

        <EncodeSettingsFields
          file={file}
          meta={meta}
          currentProfile={currentProfile}
          selected={selected}
          videoCodec={form.videoCodec}
          audioBitrateKbps={form.audioBitrateKbps}
          videoBitrates={form.videoBitrates}
          uploading={uploading}
          onCodecChange={handleCodecChange}
          onAudioBitrateChange={handleAudioBitrateChange}
          onVideoBitrateChange={handleVideoBitrateChange}
          onToggleRes={form.toggleRes}
          onSelectCurrentOnly={form.selectCurrentOnly}
          onSelectAdaptive={form.selectAdaptive}
        />

        {file && <UploadQuoteSummary quote={quote} />}

        {uploading && (
          <div className="upload-progress">
            <div>
              <span>
                {progress < 100
                  ? `Uploading source file ${progress}%`
                  : "Transcoding and preparing final quote..."}
              </span>
              <span>
                {selected.map((value) => resolutionByValue(value)?.label || value).join(", ")}
              </span>
            </div>
            <progress
              className="progress-track"
              value={progress}
              max={100}
              aria-label="Upload progress"
            />
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
