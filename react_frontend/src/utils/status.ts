export function isActiveStatus(status?: string | null): boolean {
  return !!status && ["pending", "processing", "awaiting_approval", "uploading"].includes(status);
}

export function statusLabel(status?: string | null): string {
  return (status || "").replace(/_/g, " ");
}
