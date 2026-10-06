import { create } from "zustand";
import { repoDiff, repoStatus, type RepoDiff, type RepoStatus } from "../lib/repo";
import { parseLumaError } from "../lib/hosts";

/*
 * Repository state for the directory a terminal session is sitting in.
 *
 * One target at a time (the open dialog), like the web-preview and multiplexer
 * stores. Diffs are cached per (host, cwd, path, staged) so re-selecting a file
 * — or toggling back from staged to unstaged — is instant, and the cache is
 * dropped whenever the status is refreshed, since the patches it holds may no
 * longer match the work tree.
 */

export type DiffEntry = {
  loading: boolean;
  error: string | null;
  diff: RepoDiff | null;
};

/** Cache key for one file's patch. The host and cwd are part of it so a dialog
 * opened on another session never shows a stale patch. */
export function diffKey(hostId: string, cwd: string, path: string, staged: boolean): string {
  return `${hostId}\0${cwd}\0${staged ? "s" : "u"}\0${path}`;
}

type RepoState = {
  /** Host + working directory the held status belongs to; null when closed. */
  hostId: string | null;
  cwd: string | null;
  status: RepoStatus | null;
  loading: boolean;
  error: string | null;
  diffs: Record<string, DiffEntry>;

  refresh: (hostId: string, cwd: string) => Promise<void>;
  loadDiff: (hostId: string, cwd: string, path: string, staged: boolean) => Promise<void>;
  reset: () => void;
};

export const useRepoStore = create<RepoState>((set, get) => ({
  hostId: null,
  cwd: null,
  status: null,
  loading: false,
  error: null,
  diffs: {},

  refresh: async (hostId, cwd) => {
    set({
      hostId,
      cwd,
      loading: true,
      error: null,
      status: null,
      // Patches describe a work tree that is about to be re-read.
      diffs: {},
    });
    try {
      const status = await repoStatus(hostId, cwd);
      // A later refresh for another session may have superseded this one.
      if (get().hostId !== hostId || get().cwd !== cwd) return;
      set({ status, loading: false });
    } catch (error) {
      if (get().hostId !== hostId || get().cwd !== cwd) return;
      set({ loading: false, error: parseLumaError(error).message, status: null });
    }
  },

  loadDiff: async (hostId, cwd, path, staged) => {
    const key = diffKey(hostId, cwd, path, staged);
    const cached = get().diffs[key];
    if (cached?.diff || cached?.loading) return;
    set((state) => ({
      diffs: { ...state.diffs, [key]: { loading: true, error: null, diff: null } },
    }));
    try {
      const diff = await repoDiff(hostId, cwd, path, staged);
      set((state) =>
        // Dropped by a refresh while in flight: do not resurrect the entry.
        state.diffs[key]
          ? {
              diffs: {
                ...state.diffs,
                [key]: { loading: false, error: null, diff },
              },
            }
          : state,
      );
    } catch (error) {
      const message = parseLumaError(error).message;
      set((state) =>
        state.diffs[key]
          ? {
              diffs: {
                ...state.diffs,
                [key]: { loading: false, error: message, diff: null },
              },
            }
          : state,
      );
    }
  },

  reset: () =>
    set({
      hostId: null,
      cwd: null,
      status: null,
      loading: false,
      error: null,
      diffs: {},
    }),
}));
