import { useState, type DragEvent, type RefObject } from "react";

import { formatBytes } from "../../utils/format";

interface FileDropZoneProps {
  file: File | null;
  fileInputRef: RefObject<HTMLInputElement | null>;
  onFile: (file?: File) => void;
  uploading: boolean;
}

export default function FileDropZone({ file, fileInputRef, onFile, uploading }: FileDropZoneProps) {
  const [dragging, setDragging] = useState(false);

  const onDrop = (event: DragEvent<HTMLButtonElement>) => {
    event.preventDefault();
    setDragging(false);
    onFile(event.dataTransfer.files?.[0]);
  };

  return (
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
        onChange={(event) => onFile(event.target.files?.[0])}
        disabled={uploading}
      />
      <span className="drop-icon">+</span>
      <span className="drop-title">{file ? file.name : "Drag and drop a video file"}</span>
      <span className="drop-subtitle">
        {file ? `${formatBytes(file.size)} selected` : "or click to browse from your machine"}
      </span>
    </button>
  );
}
