import { useState } from "react";
import { Modal } from "../../components/Modal";
import type { EditorOption } from "../../lib/remoteEdit";
import { cn } from "../../lib/utils";

/*
 * Which installed editor opens a remote file. The list comes from the backend's
 * detection, and only the chosen id goes back: nothing typed here can pick what
 * runs. Remounted per request (keyed by the caller), so its defaults come from
 * props rather than being synced into state.
 */
export function EditorPicker({
  fileName,
  editors,
  preferredId,
  offerRemember,
  onCancel,
  onPick,
}: {
  fileName: string;
  editors: EditorOption[];
  preferredId: string | null;
  /** Whether the choice can be remembered for every later file. */
  offerRemember: boolean;
  onCancel: () => void;
  onPick: (editorId: string, remember: boolean) => void;
}) {
  const [selected, setSelected] = useState(preferredId ?? editors[0]?.id ?? null);
  // Remembering is the default only when nothing is remembered yet; picking a
  // different editor for one file should not quietly change the default.
  const [remember, setRemember] = useState(preferredId === null);

  return (
    <Modal
      open
      onOpenChange={(open) => !open && onCancel()}
      title="Open with"
      description={fileName}
      size="sm"
      footer={
        <>
          <button
            type="button"
            onClick={onCancel}
            className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            Cancel
          </button>
          <button
            type="button"
            disabled={selected === null}
            onClick={() => selected && onPick(selected, offerRemember && remember)}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground hover:brightness-110 disabled:opacity-50"
          >
            Open
          </button>
        </>
      }
    >
      {editors.length === 0 ? (
        <div className="space-y-2 text-sm">
          <p className="text-foreground">No supported editor was found on this computer.</p>
          <p className="text-xs text-muted">
            Luma looks for Visual Studio Code, Cursor, VSCodium, Zed, Sublime Text and the system
            text editor in their standard install locations.
          </p>
        </div>
      ) : (
        <fieldset>
          <legend className="sr-only">Editor</legend>
          <div className="space-y-1">
            {editors.map((editor) => (
              <label
                key={editor.id}
                className={cn(
                  "flex cursor-pointer items-center gap-2.5 rounded-md border px-3 py-2 text-sm",
                  selected === editor.id
                    ? "border-accent bg-accent/10 text-foreground"
                    : "border-border text-foreground/90 hover:bg-raised",
                )}
              >
                <input
                  type="radio"
                  name="remote-editor"
                  value={editor.id}
                  checked={selected === editor.id}
                  onChange={() => setSelected(editor.id)}
                  className="h-3.5 w-3.5 accent-accent"
                />
                {editor.name}
              </label>
            ))}
          </div>
          {offerRemember && (
            <label className="mt-3 flex items-center gap-2 text-xs text-muted">
              <input
                type="checkbox"
                checked={remember}
                onChange={(event) => setRemember(event.target.checked)}
                className="h-3.5 w-3.5 accent-accent"
              />
              Use this editor for every file
            </label>
          )}
        </fieldset>
      )}
    </Modal>
  );
}
