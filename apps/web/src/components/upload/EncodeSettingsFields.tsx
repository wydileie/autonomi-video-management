import type { SourceVideoMeta } from "../../types";
import type { VideoCodec } from "../../hooks/useUploadWorkflow";
import { RESOLUTION_OPTIONS } from "../../constants";
import {
  optionFitsSource,
  orderedSelection,
  resolutionByValue,
  targetDimensionsForMeta,
} from "../../utils/resolutions";

interface CurrentProfile {
  label: string;
  value: string;
}

interface EncodeSettingsFieldsProps {
  audioBitrateKbps: number;
  currentProfile: CurrentProfile | null | undefined;
  file: File | null;
  meta: SourceVideoMeta | null;
  onAudioBitrateChange: (kbps: number) => void;
  onCodecChange: (codec: VideoCodec) => void;
  onSelectAdaptive: () => void;
  onSelectCurrentOnly: () => void;
  onToggleRes: (resolution: string) => void;
  onVideoBitrateChange: (resolution: string, kbps: number) => void;
  selected: string[];
  uploading: boolean;
  videoBitrates: Record<string, number>;
  videoCodec: VideoCodec;
}

export default function EncodeSettingsFields({
  file,
  meta,
  currentProfile,
  selected,
  videoCodec,
  audioBitrateKbps,
  videoBitrates,
  uploading,
  onCodecChange,
  onAudioBitrateChange,
  onVideoBitrateChange,
  onToggleRes,
  onSelectCurrentOnly,
  onSelectAdaptive,
}: EncodeSettingsFieldsProps) {
  return (
    <>
      <div className="encoding-panel">
        <div className="encoding-row">
          <label>
            <span>Video codec</span>
            <select
              value={videoCodec}
              onChange={(event) => onCodecChange(event.target.value as VideoCodec)}
              disabled={uploading}
            >
              <option value="h264">H.264</option>
              <option value="hevc">HEVC / H.265</option>
            </select>
          </label>
          <label>
            <span>Audio bitrate</span>
            <input
              type="number"
              min={32}
              max={1024}
              step={32}
              value={audioBitrateKbps}
              onChange={(event) => onAudioBitrateChange(Number(event.target.value))}
              disabled={uploading}
            />
          </label>
        </div>
        <div className="bitrate-grid">
          {orderedSelection(selected).map((resolution) => {
            const option = resolutionByValue(resolution);
            return (
              <label key={resolution}>
                <span>{option?.label || resolution}</span>
                <input
                  type="number"
                  min={64}
                  max={200000}
                  step={1}
                  value={videoBitrates[resolution] || 0}
                  onChange={(event) => onVideoBitrateChange(resolution, Number(event.target.value))}
                  disabled={uploading}
                />
              </label>
            );
          })}
        </div>
      </div>

      <div className="resolution-toolbar">
        <div>
          <span className="meta-label">Renditions to create</span>
          <p>Higher-than-source options are dimmed to avoid accidental upscales.</p>
        </div>
        <div className="quick-actions">
          <button
            type="button"
            onClick={onSelectCurrentOnly}
            disabled={!currentProfile || uploading}
          >
            Current only
          </button>
          <button type="button" onClick={onSelectAdaptive} disabled={!file || uploading}>
            Current + lower
          </button>
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
              onClick={() => !disabledBySource && onToggleRes(option.value)}
              disabled={uploading || disabledBySource}
            >
              <span className="resolution-label">{option.label}</span>
              <span>
                {targetDimensions.width} x {targetDimensions.height}
              </span>
              <span>
                {option.bitrate} · {option.note}
              </span>
              {isCurrent && <strong>Current source profile</strong>}
            </button>
          );
        })}
      </div>
    </>
  );
}
