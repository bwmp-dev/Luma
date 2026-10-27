import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  listenRemoteEdits,
  remoteEditEditors,
  remoteEditList,
  remoteEditOpen,
  remoteEditReopen,
  type EditorOption,
  type RemoteEdit,
} from "../lib/remoteEdit";
import { parseLumaError } from "../lib/hosts";

/*
 * Mirror of the backend's remote edits. The backend owns every working copy
 * and its sync state and announces each change as an event; this store holds
 * the latest snapshot per edit, plus the SFTP screen's transient state around
 * opening one: the editor picker, requests in flight, and the last open error.
 */

export type EditRequest = { sessionId: string; path: string; fileName: string };

/** What the editor picker will do once an editor is chosen. */
export type PickerTarget =
  | { kind: "open"; request: EditRequest }
  | { kind: "reopen"; editId: string; fileName: string };

type Picker = { target: PickerTarget; editors: EditorOption[]; preferredId: string | null };

type EditEvent = { kind: "changed"; edit: RemoteEdit } | { kind: "removed"; editId: string };

type RemoteEditState = {
  edits: Record<string, RemoteEdit>;
  /** File names with an open request in flight, keyed `${sessionId}:${path}`. */
  opening: Record<string, string>;
  openError: { fileName: string; message: string } | null;
  /** Why the edits couldn't be loaded or followed; `init` again to retry. */
  loadError: string | null;
  picker: Picker | null;
  init: () => Promise<void>;
  applyChange: (edit: RemoteEdit) => void;
  applyRemoval: (editId: string) => void;
  /** Open a remote file in the remembered editor, or ask which editor to use
   * when none is remembered or `choose` is set. */
  edit: (request: EditRequest, choose: boolean) => Promise<void>;
  /** Show an open file in its editor again (or a different one, if `choose`). */
  reopen: (edit: RemoteEdit, choose: boolean) => Promise<void>;
  pick: (editorId: string, remember: boolean) => Promise<void>;
  cancelPicker: () => void;
  clearOpenError: () => void;
};

/** Backend messages carry their error category as a prefix; the panel shows
 * them as sentences. */
export function editErrorMessage(error: unknown): string {
  return parseLumaError(error).message.replace(/^(invalid input|SFTP operation failed): /, "");
}

let initialized: Promise<void> | null = null;

export const useRemoteEditStore = create<RemoteEditState>((set, get) => {
  const fail = (fileName: string, error: unknown) =>
    set({ openError: { fileName, message: editErrorMessage(error) } });

  const apply = (event: EditEvent) =>
    event.kind === "changed" ? get().applyChange(event.edit) : get().applyRemoval(event.editId);

  /** A command's reply can arrive after events announcing later states of the
   * same edit, so it only fills in an edit the events have not. */
  const applyReply = (edit: RemoteEdit) =>
    set((state) =>
      edit.editId in state.edits ? {} : { edits: { ...state.edits, [edit.editId]: edit } },
    );

  const openWith = async (request: EditRequest, editorId: string, remember: boolean) => {
    const key = `${request.sessionId}:${request.path}`;
    if (key in get().opening) return;
    set((state) => ({ opening: { ...state.opening, [key]: request.fileName }, openError: null }));
    try {
      applyReply(await remoteEditOpen(request.sessionId, request.path, editorId, remember));
    } catch (error) {
      fail(request.fileName, error);
    } finally {
      set((state) => {
        const opening = { ...state.opening };
        delete opening[key];
        return { opening };
      });
    }
  };

  const reopenWith = async (editId: string, fileName: string, editorId: string) => {
    set({ openError: null });
    try {
      applyReply(await remoteEditReopen(editId, editorId));
    } catch (error) {
      fail(fileName, error);
    }
  };

  /** Detected editors are fetched every time: one can be installed or removed
   * while Luma runs. */
  const loadEditors = async (fileName: string) => {
    try {
      return await remoteEditEditors();
    } catch (error) {
      fail(fileName, error);
      return null;
    }
  };

  return {
    edits: {},
    opening: {},
    openError: null,
    loadError: null,
    picker: null,

    init: () => {
      initialized ??= (async () => {
        // Subscribe before listing so no change is missed, but hold events
        // back until the list arrives and replay them over it in order: the
        // list may predate them, and letting it land last would bring back a
        // removed edit or an outdated state.
        let held: EditEvent[] | null = [];
        const receive = (event: EditEvent) => (held ? held.push(event) : apply(event));
        let unlisten: UnlistenFn | null = null;
        try {
          unlisten = await listenRemoteEdits({
            changed: (edit) => receive({ kind: "changed", edit }),
            removed: (editId) => receive({ kind: "removed", editId }),
          });
          const edits = await remoteEditList();
          const replay = held;
          held = null;
          set({
            edits: Object.fromEntries(edits.map((edit) => [edit.editId, edit])),
            loadError: null,
          });
          replay.forEach(apply);
        } catch (error) {
          unlisten?.();
          initialized = null;
          set({ loadError: editErrorMessage(error) });
        }
      })();
      return initialized;
    },

    applyChange: (edit) => set((state) => ({ edits: { ...state.edits, [edit.editId]: edit } })),

    applyRemoval: (editId) =>
      set((state) => {
        if (!(editId in state.edits)) return {};
        const edits = { ...state.edits };
        delete edits[editId];
        return { edits };
      }),

    edit: async (request, choose) => {
      const options = await loadEditors(request.fileName);
      if (!options) return;
      if (!choose && options.preferredId) {
        await openWith(request, options.preferredId, false);
        return;
      }
      set({ picker: { target: { kind: "open", request }, ...options } });
    },

    reopen: async (edit, choose) => {
      const options = await loadEditors(edit.fileName);
      if (!options) return;
      const current = options.editors.some((editor) => editor.id === edit.editorId)
        ? edit.editorId
        : options.preferredId;
      if (!choose && current) {
        await reopenWith(edit.editId, edit.fileName, current);
        return;
      }
      set({
        picker: {
          target: { kind: "reopen", editId: edit.editId, fileName: edit.fileName },
          editors: options.editors,
          preferredId: current,
        },
      });
    },

    pick: async (editorId, remember) => {
      const picker = get().picker;
      if (!picker) return;
      set({ picker: null });
      if (picker.target.kind === "open") {
        await openWith(picker.target.request, editorId, remember);
      } else {
        await reopenWith(picker.target.editId, picker.target.fileName, editorId);
      }
    },

    cancelPicker: () => set({ picker: null }),

    clearOpenError: () => set({ openError: null }),
  };
});
