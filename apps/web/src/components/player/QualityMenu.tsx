import type { FocusEvent } from "react";

import type { VideoVariant } from "../../types";
import { variantDisplayLabel } from "../../utils/resolutions";

interface QualityMenuProps {
  onOpenChange: (open: boolean) => void;
  onSelect: (resolution: string) => void;
  onToggle: () => void;
  open: boolean;
  resolution: string;
  variants: VideoVariant[];
}

export default function QualityMenu({
  variants,
  resolution,
  open,
  onOpenChange,
  onToggle,
  onSelect,
}: QualityMenuProps) {
  const selectedLabel = variantDisplayLabel(resolution);

  return (
    <div
      className={`player-quality${open ? " open" : ""}`}
      onMouseLeave={() => onOpenChange(false)}
      onBlur={(event: FocusEvent<HTMLDivElement>) => {
        const nextTarget = event.relatedTarget instanceof Node ? event.relatedTarget : null;
        if (!event.currentTarget.contains(nextTarget)) onOpenChange(false);
      }}
    >
      <button
        type="button"
        className={`quality-toggle${open ? " active" : ""}`}
        aria-label="Quality"
        aria-expanded={open}
        aria-haspopup="menu"
        onClick={onToggle}
      >
        <span className="gear-icon" aria-hidden="true" />
        <span>{selectedLabel}</span>
      </button>
      {open && (
        <div className="quality-menu" role="menu" aria-label="Video quality">
          {variants.map((variant) => {
            const label = variantDisplayLabel(variant.resolution);
            return (
              <button
                key={variant.id}
                type="button"
                role="menuitemradio"
                aria-checked={variant.resolution === resolution}
                className={variant.resolution === resolution ? "active" : ""}
                onClick={() => onSelect(variant.resolution)}
              >
                {label}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
