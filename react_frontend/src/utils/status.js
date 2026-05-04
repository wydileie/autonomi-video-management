export function isActiveStatus(status) {
  return ["pending", "processing", "awaiting_approval", "uploading"].includes(status);
}

export function statusLabel(status) {
  return (status || "").replace(/_/g, " ");
}
