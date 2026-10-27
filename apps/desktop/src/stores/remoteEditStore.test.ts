import { beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoke } from "../test/tauriMock";
import type { RemoteEdit } from "../lib/remoteEdit";

const events = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
const listen = vi.hoisted(() =>
  vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    events.set(name, handler);
    return () => events.delete(name);
  }),
);
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const emit = (name: string, payload: unknown) => events.get(name)?.({ payload });

const { useRemoteEditStore } = await import("./remoteEditStore");

function edit(overrides: Partial<RemoteEdit> = {}): RemoteEdit {
  return {
    editId: "edit-1",
    hostId: "host-1",
    remotePath: "/etc/app.conf",
    fileName: "app.conf",
    localPath: "C:\\Luma\\remote-edits\\edit-1\\app.conf",
    editorId: "vscode",
    encoding: "utf8",
    status: "synced",
    conflict: null,
    message: null,
    unsynced: false,
    connected: true,
    lastUploadedAt: null,
    backupPath: null,
    ...overrides,
  };
}

const request = { sessionId: "sftp-1", path: "/etc/app.conf", fileName: "app.conf" };
const editors = [
  { id: "vscode", name: "Visual Studio Code" },
  { id: "notepad", name: "Notepad" },
];

beforeEach(() => {
  useRemoteEditStore.setState({
    edits: {},
    opening: {},
    openError: null,
    loadError: null,
    picker: null,
  });
});

describe("remote edit store", () => {
  it("opens straight in the remembered editor without asking", async () => {
    const calls: [string, Record<string, unknown>][] = [];
    setInvoke((cmd, args) => {
      calls.push([cmd, args]);
      if (cmd === "remote_edit_editors") return { editors, preferredId: "notepad" };
      if (cmd === "remote_edit_open") return edit({ editorId: "notepad" });
      throw new Error(`unexpected ${cmd}`);
    });

    await useRemoteEditStore.getState().edit(request, false);

    expect(calls[1]).toEqual([
      "remote_edit_open",
      { sessionId: "sftp-1", path: "/etc/app.conf", editorId: "notepad", rememberEditor: false },
    ]);
    const state = useRemoteEditStore.getState();
    expect(state.picker).toBeNull();
    expect(state.edits["edit-1"].editorId).toBe("notepad");
    expect(state.opening).toEqual({});
  });

  it("asks when nothing is remembered and remembers the choice only if told to", async () => {
    let opened: Record<string, unknown> | null = null;
    setInvoke((cmd, args) => {
      if (cmd === "remote_edit_editors") return { editors, preferredId: null };
      if (cmd === "remote_edit_open") {
        opened = args;
        return edit();
      }
      throw new Error(`unexpected ${cmd}`);
    });

    await useRemoteEditStore.getState().edit(request, false);
    expect(useRemoteEditStore.getState().picker?.target).toEqual({ kind: "open", request });
    expect(opened).toBeNull();

    await useRemoteEditStore.getState().pick("vscode", true);
    expect(opened).toMatchObject({ editorId: "vscode", rememberEditor: true });
    expect(useRemoteEditStore.getState().picker).toBeNull();
  });

  it("shows the backend's reason when a file can't be opened", async () => {
    setInvoke((cmd) => {
      if (cmd === "remote_edit_editors") return { editors, preferredId: "vscode" };
      throw {
        category: "invalid-input",
        message: "invalid input: app.bin can't be edited because it looks like a binary file",
      };
    });

    await useRemoteEditStore.getState().edit({ ...request, fileName: "app.bin" }, false);

    expect(useRemoteEditStore.getState().openError).toEqual({
      fileName: "app.bin",
      message: "app.bin can't be edited because it looks like a binary file",
    });
    expect(useRemoteEditStore.getState().opening).toEqual({});
  });

  it("reopens in a remembered editor when the edit's own one is gone", async () => {
    let reopened: Record<string, unknown> | null = null;
    setInvoke((cmd, args) => {
      if (cmd === "remote_edit_editors") return { editors, preferredId: "notepad" };
      if (cmd === "remote_edit_reopen") {
        reopened = args;
        return edit({ editorId: "notepad" });
      }
      throw new Error(`unexpected ${cmd}`);
    });

    await useRemoteEditStore.getState().reopen(edit({ editorId: "cursor" }), false);

    expect(reopened).toEqual({ editId: "edit-1", editorId: "notepad" });
  });

  it("keeps a newer event when the open reply arrives after it", async () => {
    setInvoke((cmd) => {
      if (cmd === "remote_edit_editors") return { editors, preferredId: "vscode" };
      if (cmd === "remote_edit_open") {
        useRemoteEditStore.getState().applyChange(edit({ status: "uploading", unsynced: true }));
        return edit();
      }
      throw new Error(`unexpected ${cmd}`);
    });

    await useRemoteEditStore.getState().edit(request, false);

    expect(useRemoteEditStore.getState().edits["edit-1"].status).toBe("uploading");
  });

  // One test, because loading happens once per app run.
  it("retries a failed subscription and replays events that raced the initial list", async () => {
    setInvoke((cmd) => {
      throw new Error(`unexpected ${cmd}`);
    });
    listen.mockImplementationOnce(listen.getMockImplementation()!);
    listen.mockRejectedValueOnce(new Error("event bridge unavailable"));

    await useRemoteEditStore.getState().init();
    expect(useRemoteEditStore.getState().loadError).toBe("event bridge unavailable");
    // The listener that did attach is not left behind.
    expect(events.size).toBe(0);

    setInvoke((cmd) => {
      if (cmd !== "remote_edit_list") throw new Error(`unexpected ${cmd}`);
      // Both happen after the backend took its snapshot.
      emit(
        "remote-edit://changed",
        edit({ status: "conflict", conflict: "changed", unsynced: true }),
      );
      emit("remote-edit://removed", "edit-2");
      return [edit(), edit({ editId: "edit-2", fileName: "old.conf" })];
    });

    await useRemoteEditStore.getState().init();
    const loaded = useRemoteEditStore.getState();
    expect(loaded.loadError).toBeNull();
    expect(Object.keys(loaded.edits)).toEqual(["edit-1"]);
    expect(loaded.edits["edit-1"].status).toBe("conflict");

    emit("remote-edit://changed", edit({ status: "synced" }));
    expect(useRemoteEditStore.getState().edits["edit-1"].status).toBe("synced");
    emit("remote-edit://removed", "edit-1");
    expect(useRemoteEditStore.getState().edits).toEqual({});
  });
});
