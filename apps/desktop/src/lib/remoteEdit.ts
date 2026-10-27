import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/*
 * Typed wrappers for remote file editing (desktop only). The backend downloads a
 * remote text file to a private working copy, opens it in an external editor
 * picked by id from `remoteEditEditors`, and uploads every save after checking
 * the remote still holds the version the working copy came from. State changes
 * arrive as events, so a row stays current without polling.
 */

export type RemoteEditStatus =
  | "synced"
  | "uploading"
  /** Unsynced changes, and no connected SFTP session for the host. */
  | "offline"
  | "conflict"
  | "error"
  /** Found on launch with changes that were never uploaded. */
  | "recovered";

export type RemoteEditEncoding = "utf8" | "utf8-bom" | "utf16-le" | "utf16-be";

export type RemoteEdit = {
  editId: string;
  hostId: string;
  remotePath: string;
  fileName: string;
  localPath: string;
  editorId: string | null;
  encoding: RemoteEditEncoding;
  status: RemoteEditStatus;
  conflict: "changed" | "deleted" | null;
  message: string | null;
  /** The working copy differs from the version last seen on the remote. */
  unsynced: boolean;
  connected: boolean;
  /** Unix milliseconds. */
  lastUploadedAt: number | null;
  /** Where "use remote" saved the working copy before replacing it. */
  backupPath: string | null;
};

export type EditorOption = { id: string; name: string };
export type EditorsResponse = { editors: EditorOption[]; preferredId: string | null };

export type ConflictReview = {
  remoteText: string | null;
  localText: string | null;
  remoteDeleted: boolean;
  unavailable: string | null;
};

export type ConflictResolution = "keepLocal" | "useRemote";

export const ENCODING_LABELS: Record<RemoteEditEncoding, string> = {
  utf8: "UTF-8",
  "utf8-bom": "UTF-8 with BOM",
  "utf16-le": "UTF-16 LE",
  "utf16-be": "UTF-16 BE",
};

export function remoteEditEditors(): Promise<EditorsResponse> {
  return invoke<EditorsResponse>("remote_edit_editors");
}

export function remoteEditOpen(
  sessionId: string,
  path: string,
  editorId: string,
  rememberEditor: boolean,
): Promise<RemoteEdit> {
  return invoke<RemoteEdit>("remote_edit_open", { sessionId, path, editorId, rememberEditor });
}

export function remoteEditList(): Promise<RemoteEdit[]> {
  return invoke<RemoteEdit[]>("remote_edit_list");
}

export function remoteEditReopen(editId: string, editorId: string): Promise<RemoteEdit> {
  return invoke<RemoteEdit>("remote_edit_reopen", { editId, editorId });
}

export function remoteEditReview(editId: string): Promise<ConflictReview> {
  return invoke<ConflictReview>("remote_edit_review", { editId });
}

export function remoteEditResolve(editId: string, resolution: ConflictResolution): Promise<void> {
  return invoke<void>("remote_edit_resolve", { editId, resolution });
}

export function remoteEditUpload(editId: string): Promise<void> {
  return invoke<void>("remote_edit_upload", { editId });
}

/** Rejects while the working copy has unsynced changes unless `discard`. */
export function remoteEditClose(editId: string, discard: boolean): Promise<void> {
  return invoke<void>("remote_edit_close", { editId, discard });
}

/** Select the working copy (or its last backup) in the system file manager. */
export function remoteEditReveal(editId: string, backup: boolean): Promise<void> {
  return invoke<void>("remote_edit_reveal", { editId, backup });
}

export async function listenRemoteEdits(handlers: {
  changed: (edit: RemoteEdit) => void;
  removed: (editId: string) => void;
}): Promise<UnlistenFn> {
  const unlistenChanged = await listen<RemoteEdit>("remote-edit://changed", (event) =>
    handlers.changed(event.payload),
  );
  let unlistenRemoved: UnlistenFn;
  try {
    unlistenRemoved = await listen<string>("remote-edit://removed", (event) =>
      handlers.removed(event.payload),
    );
  } catch (error) {
    unlistenChanged();
    throw error;
  }
  return () => {
    unlistenChanged();
    unlistenRemoved();
  };
}
