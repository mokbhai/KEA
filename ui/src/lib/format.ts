/** Turns a human name into a provider ref ("My Server" → "my-server"). */
export function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/**
 * Normalises a caught `unknown` into a displayable string. Tauri commands
 * reject with a plain string, so `String(e)` is the usual branch.
 */
export function toMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
