import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import {
  CheckCircle2,
  DownloadCloud,
  FileText,
  FileWarning,
  FolderOpen,
  KeyRound,
  Loader2,
} from "lucide-react";
import { Modal } from "../../components/Modal";
import {
  applyImportHosts,
  importSshConfig,
  parseLumaError,
  previewImportHosts,
  previewSshConfig,
  type ImportedHostAuthHint,
  type ImportedKeyStatus,
  type ImportSource,
  type UnlinkedKey,
} from "../../lib/hosts";
import { useInvalidateHosts } from "../../hooks/useHosts";
import { useCapabilityStore } from "../../stores/capabilityStore";
import { cn } from "../../lib/utils";

/*
 * Preview and import SSH hosts from an external source. Four sources are
 * supported:
 *   - "ssh-config": reads ~/.ssh/config in place (no file picker).
 *   - "tabby":      a Tabby config the user selects (.yaml / .yml).
 *   - "electerm":   an Electerm export the user selects (.json).
 *   - "putty":      this machine's saved PuTTY sessions, or an exported
 *                   putty.reg from another machine.
 * The backend never modifies the source; this dialog only previews candidates
 * and imports the selection. For file sources the frontend passes the absolute
 * path only — it never reads file contents, and no credentials enter state
 * beyond the .ppk passphrases the user types, which live in local state for the
 * duration of the import and are cleared when it finishes.
 */

type ImportKind = "ssh-config" | "tabby" | "electerm" | "putty";

const SOURCES: { id: ImportKind; label: string }[] = [
  { id: "ssh-config", label: "SSH config" },
  { id: "putty", label: "PuTTY" },
  { id: "tabby", label: "Tabby" },
  { id: "electerm", label: "Electerm" },
];

/** PuTTY keeps sessions in the registry, so "detect" reads machine state while
 * "file" reads a regedit export from somewhere else. */
type PuttyMode = "detect" | "file";

// A source-agnostic candidate used for rendering the selection table.
type NormalizedCandidate = {
  name: string;
  hostname: string;
  port: number;
  username: string | null;
  group: string | null;
  authHint: ImportedHostAuthHint | null;
  alreadyExists: boolean;
  keyFile: string | null;
  keyStatus: ImportedKeyStatus | null;
};

// A source-agnostic result summary.
type NormalizedResult = {
  importedCount: number;
  createdGroups: string[];
  skippedExisting: string[];
  importedKeys: string[];
  unlinkedKeys: UnlinkedKey[];
};

const AUTH_LABELS: Record<ImportedHostAuthHint, string> = {
  password: "Password",
  "public-key": "Key",
  agent: "Interactive",
  "keyboard-interactive": "Interactive",
  unknown: "Unknown",
};

/** An OpenSSH key needs no explanation — it is linked by path as always. The
 * other states change what import will do, so they are called out. */
const KEY_STATUS_LABELS: Record<ImportedKeyStatus, string | null> = {
  openssh: null,
  ppk: "PuTTY key",
  "ppk-encrypted": "PuTTY key · locked",
  missing: "Key not found",
  unreadable: "Key unreadable",
};

function fileFilters(kind: Exclude<ImportKind, "ssh-config">) {
  if (kind === "tabby") return [{ name: "Tabby config", extensions: ["yaml", "yml"] }];
  if (kind === "electerm") return [{ name: "Electerm export", extensions: ["json"] }];
  return [{ name: "PuTTY registry export", extensions: ["reg"] }];
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function ImportDialog({
  open,
  onOpenChange,
  vaultId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Vault the imported hosts land in. It also scopes the preview's
   * `alreadyExists` check, so preview and apply must be given the same one. */
  vaultId?: string;
}) {
  const invalidate = useInvalidateHosts();
  // The "SSH config" source calls ssh_config_preview/ssh_config_import, which are
  // only registered on platforms with the sshConfigImport capability (desktop). On
  // mobile that source is hidden entirely so the user can never trigger a
  // failing command; the file-picker sources (Tabby / Electerm) remain available.
  const sshConfigImport = useCapabilityStore((s) => s.capabilities.features.sshConfigImport);
  // PuTTY import needs a local .ppk to convert and a file picker, so it is
  // desktop-only for the same reason.
  const puttyImport = useCapabilityStore((s) => s.capabilities.features.puttyImport);
  const sources = useMemo(
    () =>
      SOURCES.filter(
        (s) =>
          (s.id !== "ssh-config" || sshConfigImport) && (s.id !== "putty" || puttyImport),
      ),
    [sshConfigImport, puttyImport],
  );
  const defaultSource: ImportKind = sshConfigImport
    ? "ssh-config"
    : puttyImport
      ? "putty"
      : "tabby";

  const [source, setSource] = useState<ImportKind>(defaultSource);
  const [puttyMode, setPuttyMode] = useState<PuttyMode>("detect");
  const [filePath, setFilePath] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [result, setResult] = useState<NormalizedResult | null>(null);
  const [pickError, setPickError] = useState<string | null>(null);
  // Collected in a second step, keyed by the candidate's keyFile so two hosts
  // sharing a key are only asked once.
  const [passphrases, setPassphrases] = useState<Record<string, string>>({});
  const [askingPassphrases, setAskingPassphrases] = useState(false);
  /** Which preview the default selection has already been applied for. */
  const autoSelected = useRef<string | null>(null);

  const resetTransient = () => {
    setFilePath(null);
    setSelected(new Set());
    setResult(null);
    setPickError(null);
    setPassphrases({});
    setAskingPassphrases(false);
    autoSelected.current = null;
  };

  // Reset transient state each time the dialog opens.
  useEffect(() => {
    if (open) {
      setSource(defaultSource);
      setPuttyMode("detect");
      resetTransient();
    }
  }, [open, defaultSource]);

  const usesFilePicker = source === "tabby" || source === "electerm" || (source === "putty" && puttyMode === "file");
  const previewReady = usesFilePicker ? filePath !== null : true;
  // "putty-live" reads this machine's sessions and takes no path.
  const backendSource: ImportSource | null =
    source === "ssh-config"
      ? null
      : source === "putty"
        ? puttyMode === "file"
          ? "putty"
          : "putty-live"
        : source;

  const preview = useQuery({
    queryKey: ["host-import-preview", source, puttyMode, filePath, vaultId],
    enabled: open && previewReady,
    staleTime: 0,
    gcTime: 0,
    queryFn: async (): Promise<NormalizedCandidate[]> => {
      if (backendSource === null) {
        const rows = await previewSshConfig(vaultId);
        return rows.map((c) => ({
          name: c.name,
          hostname: c.hostname,
          port: c.port,
          username: c.username,
          group: null,
          authHint: null,
          alreadyExists: c.alreadyExists,
          keyFile: null,
          keyStatus: null,
        }));
      }
      const rows = await previewImportHosts(backendSource, filePath, vaultId);
      return rows.map((c) => ({
        name: c.name,
        hostname: c.hostname,
        port: c.port,
        username: c.username,
        group: c.group,
        authHint: c.authHint,
        alreadyExists: c.alreadyExists,
        keyFile: c.keyFile,
        keyStatus: c.keyStatus,
      }));
    },
  });

  const candidates = useMemo(() => preview.data ?? [], [preview.data]);
  const importable = useMemo(
    () => candidates.filter((c) => !c.alreadyExists),
    [candidates],
  );

  // Pre-select everything importable the first time a preview arrives. Landing
  // on a fully unchecked list means the Import button is disabled, so clicking
  // it does nothing at all and reads as a broken feature. Keyed on the preview's
  // identity so a background refetch cannot undo the user's own deselections.
  const previewIdentity = `${source}:${puttyMode}:${filePath ?? ""}`;
  useEffect(() => {
    if (!preview.isSuccess || autoSelected.current === previewIdentity) return;
    autoSelected.current = previewIdentity;
    setSelected(new Set(importable.map((candidate) => candidate.name)));
  }, [preview.isSuccess, previewIdentity, importable]);
  const allSelected =
    importable.length > 0 && selected.size === importable.length;
  const hasGroups = candidates.some((c) => c.group);

  /** Distinct locked .ppk files among the selected hosts. */
  const lockedKeys = useMemo(() => {
    const paths = new Set<string>();
    for (const candidate of candidates) {
      if (selected.has(candidate.name) && candidate.keyStatus === "ppk-encrypted" && candidate.keyFile) {
        paths.add(candidate.keyFile);
      }
    }
    return [...paths];
  }, [candidates, selected]);

  const changeSource = (next: ImportKind) => {
    if (next === source) return;
    setSource(next);
    setPuttyMode("detect");
    resetTransient();
  };

  const changePuttyMode = (next: PuttyMode) => {
    if (next === puttyMode) return;
    setPuttyMode(next);
    resetTransient();
  };

  const pickFile = async () => {
    // `usesFilePicker` already excludes the sources with no file to pick.
    if (!usesFilePicker) return;
    setPickError(null);
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: fileFilters(source),
      });
      if (typeof picked === "string") {
        setFilePath(picked);
        setSelected(new Set());
        setResult(null);
      }
    } catch (error) {
      setPickError(parseLumaError(error).message);
    }
  };

  const toggle = (name: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  const toggleAll = () =>
    setSelected(
      allSelected ? new Set() : new Set(importable.map((c) => c.name)),
    );

  const runImport = useMutation({
    mutationFn: async (names: string[]): Promise<NormalizedResult> => {
      if (backendSource === null) {
        const res = await importSshConfig(names, vaultId);
        return {
          importedCount: res.importedHosts.length,
          createdGroups: [],
          skippedExisting: res.skippedExisting,
          importedKeys: [],
          unlinkedKeys: [],
        };
      }
      // Blank entries mean "skip this key": the host still imports, just
      // without its key linked.
      const supplied = Object.fromEntries(
        Object.entries(passphrases).filter(([, value]) => value.length > 0),
      );
      const res = await applyImportHosts(backendSource, filePath, names, supplied, vaultId);
      return {
        importedCount: res.importedHosts.length,
        createdGroups: res.createdGroups,
        skippedExisting: res.skippedExisting,
        importedKeys: res.importedKeys,
        unlinkedKeys: res.unlinkedKeys,
      };
    },
    onSuccess: (res) => {
      setResult(res);
      invalidate();
    },
    onSettled: () => {
      // Passphrases have done their job; do not keep them around.
      setPassphrases({});
      setAskingPassphrases(false);
    },
  });

  const submit = () => {
    // Ask for locked .ppk passphrases once, just before importing.
    if (!askingPassphrases && lockedKeys.length > 0) {
      setAskingPassphrases(true);
      return;
    }
    runImport.mutate([...selected]);
  };

  const previewError = preview.isError ? parseLumaError(preview.error) : null;
  const importError = runImport.isError ? parseLumaError(runImport.error) : null;
  const busy = runImport.isPending;

  const description =
    source === "ssh-config"
      ? "Reads ~/.ssh/config without modifying it. Select which hosts to add."
      : source === "tabby"
        ? "Import SSH hosts from a Tabby config file (.yaml). The file is never modified."
        : source === "electerm"
          ? "Import SSH hosts from an Electerm export (.json). The file is never modified."
          : "Import saved PuTTY sessions. Referenced .ppk keys are converted to OpenSSH and stored in your keychain; PuTTY's own settings are never modified.";

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="Import hosts"
      description={description}
      size="lg"
      footer={
        result ? (
          <button
            type="button"
            onClick={() => onOpenChange(false)}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground"
          >
            Done
          </button>
        ) : (
          <>
            <button
              type="button"
              onClick={() =>
                askingPassphrases ? setAskingPassphrases(false) : onOpenChange(false)
              }
              className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
            >
              {askingPassphrases ? "Back" : "Cancel"}
            </button>
            <button
              type="button"
              onClick={submit}
              disabled={selected.size === 0 || preview.isFetching || busy}
              className="flex items-center gap-1.5 rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground disabled:opacity-50"
            >
              {busy ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <DownloadCloud size={14} />
              )}
              {askingPassphrases || lockedKeys.length === 0
                ? `Import ${selected.size > 0 ? `(${selected.size})` : ""}`
                : "Continue"}
            </button>
          </>
        )
      }
    >
      {result ? (
        <div className="space-y-3">
          <div className="flex items-center gap-2 rounded-md border border-accent/40 bg-accent/10 px-3 py-2 text-sm">
            <CheckCircle2 size={16} className="shrink-0 text-accent" />
            <span>
              Imported {result.importedCount}{" "}
              {result.importedCount === 1 ? "host" : "hosts"}
              {result.importedKeys.length > 0 &&
                ` and ${result.importedKeys.length} ${result.importedKeys.length === 1 ? "key" : "keys"}`}
              {result.skippedExisting.length > 0 &&
                `, skipped ${result.skippedExisting.length} already present`}
              .
            </span>
          </div>
          {result.createdGroups.length > 0 && (
            <p className="text-xs text-muted">
              Created {result.createdGroups.length}{" "}
              {result.createdGroups.length === 1 ? "group" : "groups"}:{" "}
              {result.createdGroups.join(", ")}
            </p>
          )}
          {result.skippedExisting.length > 0 && (
            <p className="text-xs text-muted">
              Skipped: {result.skippedExisting.join(", ")}
            </p>
          )}
          {result.unlinkedKeys.length > 0 && (
            <div className="space-y-1 rounded-md border border-danger/40 bg-danger/10 px-3 py-2">
              <p className="text-xs font-medium">
                {result.unlinkedKeys.length}{" "}
                {result.unlinkedKeys.length === 1 ? "host was" : "hosts were"} imported
                without a key:
              </p>
              <ul className="space-y-0.5 text-xs text-muted">
                {result.unlinkedKeys.map((entry) => (
                  <li key={`${entry.host}:${entry.path}`}>
                    <span className="font-medium text-foreground">{entry.host}</span> —{" "}
                    {entry.reason}
                  </li>
                ))}
              </ul>
              <p className="text-[11px] text-muted">
                Add the key from the keychain, then set it on the host.
              </p>
            </div>
          )}
        </div>
      ) : askingPassphrases ? (
        /* Passphrase step ---------------------------------------------- */
        <div className="space-y-3">
          <p className="text-sm text-muted">
            {lockedKeys.length === 1
              ? "One selected host uses a passphrase-protected PuTTY key."
              : `${lockedKeys.length} selected hosts use passphrase-protected PuTTY keys.`}{" "}
            Enter the passphrase to convert and store the key. Leave it blank to
            import the host without its key.
          </p>
          <ul className="space-y-3">
            {lockedKeys.map((path) => (
              <li key={path} className="space-y-1">
                <label className="flex items-center gap-2 text-xs text-muted">
                  <KeyRound size={13} className="shrink-0 text-accent" />
                  <span className="truncate font-mono" title={path}>
                    {basename(path)}
                  </span>
                </label>
                <input
                  type="password"
                  autoComplete="off"
                  value={passphrases[path] ?? ""}
                  onChange={(event) =>
                    setPassphrases((prev) => ({ ...prev, [path]: event.target.value }))
                  }
                  placeholder="Passphrase"
                  className="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm outline-none focus:border-accent"
                />
              </li>
            ))}
          </ul>
          {importError && <ImportErrorBanner error={importError} />}
        </div>
      ) : (
        <div className="space-y-4">
          {/* Source selector -------------------------------------------- */}
          <div
            role="tablist"
            aria-label="Import source"
            className="flex gap-1 rounded-lg border border-border bg-background p-1"
          >
            {sources.map((s) => (
              <button
                key={s.id}
                type="button"
                role="tab"
                aria-selected={source === s.id}
                onClick={() => changeSource(s.id)}
                className={cn(
                  "flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
                  source === s.id
                    ? "bg-accent text-accent-foreground"
                    : "text-muted hover:text-foreground",
                )}
              >
                {s.label}
              </button>
            ))}
          </div>

          {/* PuTTY: detect installed sessions, or read an export --------- */}
          {source === "putty" && (
            <div
              role="radiogroup"
              aria-label="PuTTY session source"
              className="flex gap-2"
            >
              {(
                [
                  { id: "detect", label: "Detect installed sessions" },
                  { id: "file", label: "Choose a putty.reg export" },
                ] as { id: PuttyMode; label: string }[]
              ).map((option) => (
                <button
                  key={option.id}
                  type="button"
                  role="radio"
                  aria-checked={puttyMode === option.id}
                  onClick={() => changePuttyMode(option.id)}
                  className={cn(
                    "flex-1 rounded-md border px-3 py-1.5 text-xs transition-colors",
                    puttyMode === option.id
                      ? "border-accent text-foreground"
                      : "border-border text-muted hover:text-foreground",
                  )}
                >
                  {option.label}
                </button>
              ))}
            </div>
          )}

          {/* File picker ------------------------------------------------ */}
          {usesFilePicker && (
            <div className="space-y-2">
              <button
                type="button"
                onClick={() => void pickFile()}
                className="flex w-full items-center gap-2 rounded-md border border-border bg-background px-3 py-2 text-sm text-muted hover:border-accent hover:text-foreground"
              >
                <FolderOpen size={15} className="shrink-0 text-accent" />
                {filePath ? "Choose a different file…" : "Choose file…"}
              </button>
              {filePath && (
                <div className="flex items-center gap-2 rounded-md bg-raised px-3 py-1.5 text-xs text-muted">
                  <FileText size={13} className="shrink-0" />
                  <span className="truncate font-mono" title={filePath}>
                    {basename(filePath)}
                  </span>
                </div>
              )}
              {pickError && (
                <p className="text-xs text-danger">
                  Could not open file picker: {pickError}
                </p>
              )}
            </div>
          )}

          {/* Preview states --------------------------------------------- */}
          {usesFilePicker && !filePath ? (
            <p className="rounded-md border border-dashed border-border px-3 py-6 text-center text-sm text-muted">
              Choose a{" "}
              {source === "tabby"
                ? "Tabby config (.yaml)"
                : source === "electerm"
                  ? "Electerm export (.json)"
                  : "PuTTY export (.reg)"}{" "}
              file to preview its hosts.
            </p>
          ) : preview.isLoading ? (
            <p className="flex items-center gap-2 text-sm text-muted">
              <Loader2 size={14} className="animate-spin" />
              {source === "ssh-config"
                ? "Reading SSH config…"
                : source === "putty" && puttyMode === "detect"
                  ? "Looking for saved PuTTY sessions…"
                  : "Reading file…"}
            </p>
          ) : previewError ? (
            <div className="flex items-start gap-2 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              <FileWarning size={15} className="mt-0.5 shrink-0" />
              <span>
                {source === "ssh-config"
                  ? "Could not read SSH config: "
                  : source === "putty" && puttyMode === "detect"
                    ? "Could not read PuTTY sessions: "
                    : "Could not read file: "}
                {previewError.message}
              </span>
            </div>
          ) : candidates.length === 0 ? (
            source === "putty" ? (
              /* An empty PuTTY result is usually not a fault: PuTTY writes a
                 session only when one is explicitly saved, so a heavy user who
                 always types the hostname has none at all. Saying just "none
                 found" reads like the feature is broken. */
              <div className="space-y-2 rounded-md border border-dashed border-border px-4 py-6 text-center">
                <p className="text-sm">
                  {puttyMode === "detect"
                    ? "No saved PuTTY sessions on this device."
                    : "No PuTTY sessions in this file."}
                </p>
                <p className="text-xs text-muted">
                  {puttyMode === "detect"
                    ? "PuTTY only stores a session when you save one in its configuration window — connecting by typing a hostname does not create one."
                    : "The export needs to include HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY."}
                </p>
                <button
                  type="button"
                  onClick={() =>
                    changePuttyMode(puttyMode === "detect" ? "file" : "detect")
                  }
                  className="text-xs font-medium text-accent hover:underline"
                >
                  {puttyMode === "detect"
                    ? "Import a putty.reg export instead"
                    : "Look for sessions on this device instead"}
                </button>
              </div>
            ) : (
              <p className="rounded-md border border-dashed border-border px-3 py-6 text-center text-sm text-muted">
                {source === "ssh-config"
                  ? "No hosts found in ~/.ssh/config."
                  : "No SSH hosts found in this file."}
              </p>
            )
          ) : (
            <div className="space-y-2">
              <div className="flex items-center justify-between px-1">
                <label className="flex cursor-pointer items-center gap-2 text-xs text-muted">
                  <input
                    type="checkbox"
                    checked={allSelected}
                    onChange={toggleAll}
                    disabled={importable.length === 0}
                    className="h-3.5 w-3.5 accent-accent"
                  />
                  Select all importable
                </label>
                <span className="text-xs text-muted">
                  {candidates.length} found
                </span>
              </div>
              <ul className="divide-y divide-border rounded-md border border-border">
                {candidates.map((c) => {
                  const disabled = c.alreadyExists;
                  const keyLabel = c.keyStatus ? KEY_STATUS_LABELS[c.keyStatus] : null;
                  const keyMissing =
                    c.keyStatus === "missing" || c.keyStatus === "unreadable";
                  return (
                    <li key={c.name}>
                      <label
                        className={cn(
                          "flex items-center gap-3 px-3 py-2 text-sm",
                          disabled
                            ? "cursor-not-allowed opacity-60"
                            : "cursor-pointer hover:bg-raised",
                        )}
                      >
                        <input
                          type="checkbox"
                          checked={selected.has(c.name)}
                          disabled={disabled}
                          onChange={() => toggle(c.name)}
                          className="h-4 w-4 accent-accent"
                        />
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-2">
                            <p className="truncate font-medium">{c.name}</p>
                            {c.group && (
                              <span className="shrink-0 rounded bg-raised px-1.5 py-0.5 text-[11px] text-muted">
                                {c.group}
                              </span>
                            )}
                          </div>
                          <p className="truncate font-mono text-xs text-muted">
                            {c.username ? `${c.username}@` : ""}
                            {c.hostname}:{c.port}
                          </p>
                        </div>
                        {keyLabel && (
                          <span
                            className={cn(
                              "shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium",
                              keyMissing
                                ? "bg-danger/15 text-danger"
                                : "bg-raised text-muted",
                            )}
                          >
                            {keyLabel}
                          </span>
                        )}
                        {c.authHint && (
                          <span className="shrink-0 rounded bg-accent/15 px-1.5 py-0.5 text-[11px] font-medium text-accent">
                            {AUTH_LABELS[c.authHint]}
                          </span>
                        )}
                        {disabled && (
                          <span className="shrink-0 rounded bg-raised px-1.5 py-0.5 text-[11px] text-muted">
                            Already added
                          </span>
                        )}
                      </label>
                    </li>
                  );
                })}
              </ul>
              {hasGroups && (
                <p className="px-1 text-[11px] text-muted">
                  Groups shown as badges are created automatically on import.
                </p>
              )}
            </div>
          )}

          {/* Pinned above the footer rather than appended to the list: the
              Import button lives in the fixed footer, so an error tacked onto
              the end of a long scrolling list is off-screen at the moment it
              appears — the import looks like it silently did nothing. */}
          {importError && <ImportErrorBanner error={importError} />}
        </div>
      )}
    </Modal>
  );
}

function ImportErrorBanner({
  error,
}: {
  error: { category: string; message: string };
}) {
  return (
    <div
      role="alert"
      className="sticky bottom-0 flex items-start gap-2 rounded-md border border-danger/40 bg-surface px-3 py-2 text-xs text-danger shadow-lg"
    >
      <FileWarning size={14} className="mt-px shrink-0" />
      <span>
        {error.category === "keystore-locked"
          ? "Unlock your keychain before importing PuTTY keys."
          : `Import failed: ${error.message}`}
      </span>
    </div>
  );
}
