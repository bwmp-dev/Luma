// Convert the `file://` URLs returned by iOS dialogs into native paths.
export function normalizeDialogPath(path: string): string {
  if (path.slice(0, 5).toLowerCase() !== "file:") return path;

  try {
    const url = new URL(path);
    if (url.protocol !== "file:" || (url.hostname && url.hostname !== "localhost")) {
      return path;
    }
    return decodeURIComponent(url.pathname);
  } catch {
    return path;
  }
}
