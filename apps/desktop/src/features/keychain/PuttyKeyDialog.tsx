import { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { Fingerprint, FolderOpen, KeyRound, Loader2, Lock, X } from "lucide-react";
import {
  importPuttyKey,
  inspectPuttyKey,
  parseLumaError,
  type PuttyKeyInfo,
} from "../../lib/hosts";

/*
 * Import a PuTTY private key (.ppk).
 *
 * The file is inspected first so the key can be described before anything is
 * asked of the user, and so a passphrase is only requested when the key
 * actually has one. The frontend passes the path, never the file contents; the
 * passphrase lives in local state only until the import resolves.
 *
 * PuTTY's container cannot be read by russh, so the backend converts the key to
 * OpenSSH before storing it. What lands in the keychain is a normal
 * keystore-backed key, indistinguishable from a generated one.
 */
export function PuttyKeyDialog({
  open,
  onOpenChange,
  onImported,
  vaultId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onImported: () => void;
  vaultId: string;
}) {
  const [path, setPath] = useState<string | null>(null);
  const [info, setInfo] = useState<PuttyKeyInfo | null>(null);
  const [name, setName] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [inspecting, setInspecting] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setPath(null);
      setInfo(null);
      setName("");
      setPassphrase("");
      setInspecting(false);
      setImporting(false);
      setError(null);
    }
  }, [open]);

  const pickFile = async () => {
    setError(null);
    let picked: string | string[] | null;
    try {
      picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "PuTTY private key", extensions: ["ppk"] }],
      });
    } catch (value) {
      setError(parseLumaError(value).message);
      return;
    }
    if (typeof picked !== "string") return;

    setPath(picked);
    setInfo(null);
    setPassphrase("");
    setInspecting(true);
    try {
      const inspected = await inspectPuttyKey(picked);
      setInfo(inspected);
      // The key's own comment is almost always the name the user wants.
      setName(inspected.comment.trim());
    } catch (value) {
      setError(parseLumaError(value).message);
    } finally {
      setInspecting(false);
    }
  };

  const submit = async () => {
    if (!path || !info) return;
    setImporting(true);
    setError(null);
    try {
      await importPuttyKey({
        path,
        name: name.trim() || null,
        passphrase: info.encrypted ? passphrase : null,
        vaultId,
      });
      onImported();
      onOpenChange(false);
    } catch (value) {
      const parsed = parseLumaError(value);
      setError(
        parsed.category === "keystore-locked"
          ? "Unlock your keychain before importing a key."
          : parsed.message,
      );
    } finally {
      setImporting(false);
      setPassphrase("");
    }
  };

  const ready = info !== null && (!info.encrypted || passphrase.length > 0);

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-black/55" />
        <Dialog.Content
          aria-describedby={undefined}
          className="fixed left-1/2 top-1/2 z-50 flex max-h-[80vh] w-[min(92vw,34rem)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-border bg-surface shadow-glow focus:outline-none"
        >
          <header className="flex items-start gap-3 border-b border-border px-5 py-4">
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent/15 text-accent">
              <KeyRound size={18} />
            </span>
            <div className="min-w-0 flex-1">
              <Dialog.Title className="text-sm font-semibold">
                Import PuTTY key
              </Dialog.Title>
              <p className="mt-0.5 text-xs text-muted">
                The key is converted to OpenSSH format and stored in your
                keychain. The .ppk file is left untouched.
              </p>
            </div>
            <Dialog.Close className="rounded p-1 text-muted hover:bg-raised hover:text-foreground">
              <X size={16} />
            </Dialog.Close>
          </header>

          <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
            <button
              type="button"
              onClick={() => void pickFile()}
              disabled={inspecting || importing}
              className="flex w-full items-center gap-2 rounded-md border border-border bg-background px-3 py-2 text-sm text-muted hover:border-accent hover:text-foreground disabled:opacity-50"
            >
              <FolderOpen size={15} className="shrink-0 text-accent" />
              {path ? "Choose a different key…" : "Choose a .ppk file…"}
            </button>

            {error && (
              <div
                role="alert"
                className="rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger"
              >
                {error}
              </div>
            )}

            {inspecting && (
              <p className="flex items-center gap-2 text-sm text-muted">
                <Loader2 size={14} className="animate-spin" /> Reading key…
              </p>
            )}

            {info && (
              <>
                <div className="space-y-1.5 rounded-lg bg-raised px-3 py-3">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-xs font-semibold">
                      {info.comment || info.algorithm}
                    </span>
                    <span className="shrink-0 rounded bg-accent/15 px-1.5 py-0.5 text-[10px] font-medium text-accent">
                      {info.algorithm}
                    </span>
                    {info.encrypted && (
                      <span className="flex shrink-0 items-center gap-1 rounded bg-background px-1.5 py-0.5 text-[10px] text-muted">
                        <Lock size={9} /> Encrypted
                      </span>
                    )}
                  </div>
                  <span className="flex items-center gap-1 truncate font-mono text-[10px] text-muted">
                    <Fingerprint size={10} /> {info.fingerprint}
                  </span>
                  <span className="block text-[10px] text-muted">
                    PuTTY key format {info.version}
                  </span>
                </div>

                <label className="block text-xs text-muted">
                  Name
                  <input
                    value={name}
                    onChange={(event) => setName(event.target.value)}
                    placeholder={info.algorithm}
                    className="mt-1 w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm text-foreground outline-none focus:border-accent"
                  />
                </label>

                {info.encrypted && (
                  <label className="block text-xs text-muted">
                    Passphrase
                    <input
                      type="password"
                      autoComplete="off"
                      value={passphrase}
                      onChange={(event) => setPassphrase(event.target.value)}
                      className="mt-1 w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm text-foreground outline-none focus:border-accent"
                    />
                    <span className="mt-1 block text-[10px] text-muted">
                      The converted key keeps this passphrase, so it stays as
                      protected as the original.
                    </span>
                  </label>
                )}
              </>
            )}
          </div>

          <footer className="flex justify-end gap-2 border-t border-border px-5 py-3">
            <button
              type="button"
              onClick={() => onOpenChange(false)}
              className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!ready || importing}
              className="flex items-center gap-1.5 rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground disabled:opacity-50"
            >
              {importing && <Loader2 size={14} className="animate-spin" />}
              Import
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
