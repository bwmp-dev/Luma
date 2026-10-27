import { useMemo, useState } from "react";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import { Modal } from "../../components/Modal";
import { diffLines } from "../../lib/lineDiff";
import {
  remoteEditResolve,
  remoteEditReview,
  type ConflictResolution,
  type ConflictReview,
  type RemoteEdit,
} from "../../lib/remoteEdit";
import { editErrorMessage } from "../../stores/remoteEditStore";
import { UnifiedHunk } from "../repo/RepoDialog";

/*
 * Review of a paused upload: what changed on the remote since the working copy
 * was downloaded, against what the user saved, and the two ways out. Leaving
 * the dialog decides nothing — uploads stay paused until a choice is made.
 */

/** Past this many characters a line diff would stall the window. */
const MAX_DIFF_CHARS = 1024 * 1024;

type DiffResult = NonNullable<ReturnType<typeof diffLines>> | "too-large" | "too-different" | null;

export function RemoteEditConflict({
  edit,
  hostName,
  onClose,
}: {
  edit: RemoteEdit;
  hostName: string;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState<ConflictResolution | null>(null);
  const [error, setError] = useState<string | null>(null);
  const deleted = edit.conflict === "deleted";
  const keepLabel = deleted ? "Recreate with my version" : "Keep my version";

  const review = useQuery({
    queryKey: ["remote-edit-review", edit.editId],
    queryFn: () => remoteEditReview(edit.editId),
    gcTime: 0,
    refetchOnMount: "always",
    retry: false,
  });

  const diff = useMemo((): DiffResult => {
    const data = review.data;
    if (!data || data.remoteText === null || data.localText === null) return null;
    if (data.remoteText.length + data.localText.length > MAX_DIFF_CHARS) return "too-large";
    return diffLines(data.remoteText, data.localText) ?? "too-different";
  }, [review.data]);

  const resolve = async (resolution: ConflictResolution) => {
    setBusy(resolution);
    setError(null);
    try {
      await remoteEditResolve(edit.editId, resolution);
      onClose();
    } catch (failure) {
      setError(editErrorMessage(failure));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Modal
      open
      onOpenChange={(open) => !open && onClose()}
      title={
        deleted
          ? `${edit.fileName} was deleted on ${hostName}`
          : `${edit.fileName} changed on ${hostName}`
      }
      description={edit.remotePath}
      size="xl"
      footer={
        <>
          <button
            type="button"
            onClick={onClose}
            className="mr-auto rounded-md px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            Decide later
          </button>
          {!deleted && (
            <button
              type="button"
              disabled={busy !== null}
              onClick={() => void resolve("useRemote")}
              className="rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent disabled:opacity-50"
            >
              {busy === "useRemote" ? "Replacing…" : "Use remote version"}
            </button>
          )}
          <button
            type="button"
            disabled={busy !== null}
            onClick={() => void resolve("keepLocal")}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground hover:brightness-110 disabled:opacity-50"
          >
            {busy === "keepLocal" ? "Uploading…" : keepLabel}
          </button>
        </>
      }
    >
      <div className="space-y-4 text-sm">
        <p className="text-muted">
          {deleted
            ? "The file was deleted or moved after you opened it, so your last save was not uploaded."
            : "Someone changed the file after you opened it, so your last save was not uploaded. Nothing has been overwritten on either side."}
        </p>

        <ConflictBody review={review} diff={diff} deleted={deleted} />

        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <dt className="font-medium text-foreground">{keepLabel}</dt>
          <dd className="text-muted">
            {deleted
              ? `Uploads your working copy to ${edit.remotePath}.`
              : "Uploads your working copy over the remote file."}
          </dd>
          {!deleted && (
            <>
              <dt className="font-medium text-foreground">Use remote version</dt>
              <dd className="text-muted">
                Replaces your working copy with the remote file. Your version is saved as a backup
                first.
              </dd>
            </>
          )}
        </dl>

        <p className="text-xs text-muted">
          Luma compares the file's contents before every upload and again just before replacing the
          file. SFTP can't lock a file, so a write that lands in the moment between that last check
          and the replacement can still be overwritten.
        </p>

        {error && (
          <p role="alert" className="text-xs text-danger">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}

function ConflictBody({
  review,
  diff,
  deleted,
}: {
  review: UseQueryResult<ConflictReview>;
  diff: DiffResult;
  deleted: boolean;
}) {
  if (review.isPending) {
    return <p className="text-xs text-muted">Comparing versions…</p>;
  }
  if (review.isError) {
    return <Notice>Couldn't load the versions to compare: {editErrorMessage(review.error)}</Notice>;
  }
  if (deleted) return null;
  if (review.data.unavailable) return <Notice>{review.data.unavailable}</Notice>;
  if (diff === "too-large") {
    return <Notice>These files are too large to compare here. Compare them in your editor.</Notice>;
  }
  if (diff === "too-different") {
    return <Notice>The versions differ in too many lines to show here.</Notice>;
  }
  if (diff === null || diff.length === 0) {
    return <Notice>The versions differ only in ways a line diff can't show.</Notice>;
  }
  return (
    <div className="overflow-hidden rounded-md border border-border">
      <div className="flex items-center gap-4 border-b border-border bg-raised px-2 py-1.5 text-[11px] text-muted">
        <span>
          <span className="font-mono text-diff-remove">−</span> Remote version
        </span>
        <span>
          <span className="font-mono text-diff-add">+</span> Your version
        </span>
      </div>
      <div className="max-h-[45vh] overflow-auto">
        {diff.map((hunk, index) => (
          <div key={`${hunk.header}:${index}`}>
            <div className="bg-raised px-2 py-1 font-mono text-[11px] text-muted">
              {hunk.header}
            </div>
            <UnifiedHunk hunk={hunk} />
          </div>
        ))}
      </div>
    </div>
  );
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <p className="flex items-start gap-2 rounded-md border border-border bg-raised px-3 py-2 text-xs text-muted">
      <AlertTriangle size={14} className="mt-px shrink-0 text-warning" />
      <span>{children}</span>
    </p>
  );
}
