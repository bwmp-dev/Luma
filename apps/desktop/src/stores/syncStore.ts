import { create } from "zustand";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { parseLumaError } from "../lib/hosts";
import { queryClient } from "../lib/queryClient";
import { SYNC_CONFIG_KEY } from "../hooks/useSync";
import {
  AUTO_SYNC_EVENT,
  syncNow as runSyncNow,
  syncResolve as runSyncResolve,
  syncSetPassphrase,
  type AutoSyncEvent,
  type Conflict,
  type ConflictResolution,
  type SyncReport,
} from "../lib/sync";

/*
 * Shared sync runtime state, keyed by vault. Each vault has its own remote,
 * passphrase and baseline, so a conflict or error in one must leave the others
 * running. `activeVaultId` names the vault whose conflicts or passphrase prompt
 * the shared dialogs are currently showing.
 *
 * Pending conflicts live only in backend memory across restarts, so this store
 * simply mirrors the most recent report per vault.
 *
 * SECURITY: no secrets are ever held in this store. Passphrases pass through
 * `submitPassphrase` as a transient argument and are never persisted here.
 */

export type SyncRuntimeStatus = "idle" | "syncing" | "error" | "conflict";

/**
 * Query keys refreshed after a pull applies remote changes. Entity keys carry
 * the vault id as their second element, so these bare prefixes match every
 * vault's cached list.
 */
const PULL_INVALIDATION_KEYS = [
  ["hosts"],
  ["recent-hosts"],
  ["host-groups"],
  ["key-references"],
  ["identities"],
  ["profiles"],
  ["settings"],
  ["snippets"],
];

export type VaultSyncState = {
  status: SyncRuntimeStatus;
  /** Pending conflicts from the last sync/resolve (empty when none). */
  conflicts: Conflict[];
  /** The most recent successful report (for pulled/pushed/upToDate feedback). */
  lastReport: SyncReport | null;
  errorCategory: string | null;
  errorMessage: string | null;
  /** True when sync failed because this vault's passphrase is not loaded. */
  needsPassphrase: boolean;
  /** Set-passphrase / resolve in-flight flag distinct from `status`. */
  busy: boolean;
  /** True while the *backend scheduler* is driving this vault's sync. */
  automatic: boolean;
};

export const EMPTY_VAULT_SYNC_STATE: VaultSyncState = {
  status: "idle",
  conflicts: [],
  lastReport: null,
  errorCategory: null,
  errorMessage: null,
  needsPassphrase: false,
  busy: false,
  automatic: false,
};

type SyncState = {
  byVault: Record<string, VaultSyncState>;
  /** The vault the shared conflict / passphrase dialogs are showing. */
  activeVaultId: string | null;
  conflictDialogOpen: boolean;
  passphraseDialogOpen: boolean;

  /** Run a sync. On a missing passphrase, opens the prompt for that vault. */
  syncNow: (vaultId: string) => Promise<void>;
  /** Submit resolutions for every displayed conflict of one vault at once. */
  resolve: (vaultId: string, resolutions: ConflictResolution[]) => Promise<void>;
  /** Load a vault's passphrase (optionally remembering it) then retry its sync. */
  submitPassphrase: (
    vaultId: string,
    passphrase: string,
    remember: boolean,
  ) => Promise<void>;
  /** Apply one scheduler event (see `startAutoSyncListener`). */
  applyAutoSyncEvent: (event: AutoSyncEvent) => void;
  /** Title-bar entry point: surface whichever vault needs attention, else sync all. */
  activate: (vaultIds: string[]) => void;
  openConflicts: (vaultId: string) => void;
  closeConflicts: () => void;
  openPassphrasePrompt: (vaultId: string) => void;
  closePassphrasePrompt: () => void;
  clearError: (vaultId: string) => void;
  /** Clear one vault's runtime state (called when its sync is disabled). */
  reset: (vaultId: string) => void;
  /** Clear every vault's runtime state. */
  resetAll: () => void;
};

/** One vault's state, or the empty state when it has never synced. */
export function selectVault(state: SyncState, vaultId: string | null): VaultSyncState {
  if (!vaultId) return EMPTY_VAULT_SYNC_STATE;
  return state.byVault[vaultId] ?? EMPTY_VAULT_SYNC_STATE;
}

const STATUS_PRIORITY: Record<SyncRuntimeStatus, number> = {
  conflict: 3,
  error: 2,
  syncing: 1,
  idle: 0,
};

/** The most urgent status across every vault: conflict > error > syncing > idle. */
export function selectAggregateStatus(state: SyncState): SyncRuntimeStatus {
  let worst: SyncRuntimeStatus = "idle";
  for (const vault of Object.values(state.byVault)) {
    if (STATUS_PRIORITY[vault.status] > STATUS_PRIORITY[worst]) worst = vault.status;
  }
  return worst;
}

/** Total pending conflicts across every vault. */
export function selectTotalConflictCount(state: SyncState): number {
  let total = 0;
  for (const vault of Object.values(state.byVault)) total += vault.conflicts.length;
  return total;
}

/** The vault that most needs attention, or null when none does. */
export function selectAttentionVaultId(state: SyncState): string | null {
  let needsPassphrase: string | null = null;
  for (const [vaultId, vault] of Object.entries(state.byVault)) {
    if (vault.conflicts.length > 0) return vaultId;
    if (vault.needsPassphrase && !needsPassphrase) needsPassphrase = vaultId;
  }
  return needsPassphrase;
}

type SetState = (updater: (state: SyncState) => Partial<SyncState>) => void;

function patchVault(
  set: SetState,
  vaultId: string,
  patch: Partial<VaultSyncState>,
) {
  set((state) => ({
    byVault: {
      ...state.byVault,
      [vaultId]: { ...selectVault(state, vaultId), ...patch },
    },
  }));
}

/**
 * Whether the user is watching. A sync they asked for may open a dialog on its
 * result; one the scheduler started may not — a modal appearing over the
 * terminal because a timer fired would be worse than the problem it reports.
 * Background conflicts and errors still land in the store, so the title-bar
 * indicator and the vault's sync panel show them.
 */
type ReportOrigin = "user" | "background";

function applyReport(
  report: SyncReport,
  vaultId: string,
  set: SetState,
  origin: ReportOrigin = "user",
) {
  if (report.pulled) {
    for (const key of PULL_INVALIDATION_KEYS) {
      void queryClient.invalidateQueries({ queryKey: key });
    }
  }
  // The config's lastSyncAt / lastRemoteVersion changed after any sync attempt.
  void queryClient.invalidateQueries({ queryKey: SYNC_CONFIG_KEY });

  const conflicted = report.conflicts.length > 0;
  patchVault(set, vaultId, {
    status: conflicted ? "conflict" : "idle",
    conflicts: report.conflicts,
    lastReport: report,
    errorCategory: null,
    errorMessage: null,
    automatic: false,
  });
  set((state) => {
    if (conflicted) {
      if (origin === "background") return {};
      return { activeVaultId: vaultId, conflictDialogOpen: true };
    }
    // Only close the dialog if it was showing this vault's conflicts.
    if (state.activeVaultId !== vaultId) return {};
    return { conflictDialogOpen: false };
  });
}

function handleError(
  error: unknown,
  vaultId: string,
  set: SetState,
  origin: ReportOrigin = "user",
) {
  const { category, message } = parseLumaError(error);
  // A missing sync passphrase is recoverable in place; a locked device keystore
  // is a different condition with a different remedy, so it falls through to
  // the plain error path.
  if (category === "sync-passphrase-required") {
    patchVault(set, vaultId, {
      status: "error",
      needsPassphrase: true,
      errorCategory: category,
      errorMessage: null,
      automatic: false,
    });
    if (origin === "user") {
      set(() => ({ activeVaultId: vaultId, passphraseDialogOpen: true }));
    }
    return;
  }
  const friendly =
    category === "sync-conflict"
      ? "Remote changed during sync — try again."
      : message;
  patchVault(set, vaultId, {
    status: "error",
    errorCategory: category,
    errorMessage: friendly,
    automatic: false,
  });
}

export const useSyncStore = create<SyncState>((set, get) => ({
  byVault: {},
  activeVaultId: null,
  conflictDialogOpen: false,
  passphraseDialogOpen: false,

  syncNow: async (vaultId) => {
    if (selectVault(get(), vaultId).status === "syncing") return;
    patchVault(set, vaultId, {
      status: "syncing",
      errorCategory: null,
      errorMessage: null,
      automatic: false,
    });
    try {
      applyReport(await runSyncNow(vaultId), vaultId, set);
    } catch (error) {
      handleError(error, vaultId, set);
    }
  },

  resolve: async (vaultId, resolutions) => {
    patchVault(set, vaultId, { busy: true, errorCategory: null, errorMessage: null });
    try {
      applyReport(await runSyncResolve(vaultId, resolutions), vaultId, set);
    } catch (error) {
      handleError(error, vaultId, set);
    } finally {
      patchVault(set, vaultId, { busy: false });
    }
  },

  submitPassphrase: async (vaultId, passphrase, remember) => {
    patchVault(set, vaultId, { busy: true, errorCategory: null, errorMessage: null });
    try {
      await syncSetPassphrase(vaultId, passphrase, remember);
      void queryClient.invalidateQueries({ queryKey: SYNC_CONFIG_KEY });
      patchVault(set, vaultId, { needsPassphrase: false, busy: false });
      set((state) =>
        state.activeVaultId === vaultId ? { passphraseDialogOpen: false } : {},
      );
      // Retry the sync once now that a passphrase is loaded.
      await get().syncNow(vaultId);
    } catch (error) {
      handleError(error, vaultId, set);
      patchVault(set, vaultId, { busy: false });
    }
  },

  applyAutoSyncEvent: (event) => {
    const { vaultId, phase } = event;
    if (phase === "started") {
      patchVault(set, vaultId, {
        status: "syncing",
        errorCategory: null,
        errorMessage: null,
        automatic: true,
      });
      return;
    }
    if (phase === "completed" && event.report) {
      applyReport(event.report, vaultId, set, "background");
      return;
    }
    // A failed attempt carries the backend's category and message directly
    // rather than a rejected promise, so rebuild the shape parseLumaError reads.
    handleError(
      {
        category: event.errorCategory ?? "unknown",
        message: event.errorMessage ?? "Automatic sync failed.",
      },
      vaultId,
      set,
      "background",
    );
  },

  activate: (vaultIds) => {
    const state = get();
    const attention = selectAttentionVaultId(state);
    if (attention) {
      const vault = selectVault(state, attention);
      set(() => ({
        activeVaultId: attention,
        conflictDialogOpen: vault.conflicts.length > 0,
        passphraseDialogOpen: vault.conflicts.length === 0,
      }));
      return;
    }
    for (const vaultId of vaultIds) void state.syncNow(vaultId);
  },

  openConflicts: (vaultId) =>
    set(() => ({ activeVaultId: vaultId, conflictDialogOpen: true })),
  closeConflicts: () => set(() => ({ conflictDialogOpen: false })),
  openPassphrasePrompt: (vaultId) =>
    set(() => ({ activeVaultId: vaultId, passphraseDialogOpen: true })),
  closePassphrasePrompt: () => set(() => ({ passphraseDialogOpen: false })),
  clearError: (vaultId) =>
    patchVault(set, vaultId, { errorCategory: null, errorMessage: null }),

  reset: (vaultId) =>
    set((state) => {
      const { [vaultId]: _removed, ...rest } = state.byVault;
      if (state.activeVaultId !== vaultId) return { byVault: rest };
      return {
        byVault: rest,
        activeVaultId: null,
        conflictDialogOpen: false,
        passphraseDialogOpen: false,
      };
    }),

  resetAll: () =>
    set(() => ({
      byVault: {},
      activeVaultId: null,
      conflictDialogOpen: false,
      passphraseDialogOpen: false,
    })),
}));

/**
 * Mirror the backend scheduler's activity into this store. Automatic syncs run
 * in Rust (see `sync/auto.rs`) so they keep working while the window is hidden;
 * these events are how the UI learns what happened. Nothing here starts a sync.
 */
export function startAutoSyncListener(): () => void {
  let unlisten: (() => void) | undefined;
  let cancelled = false;
  void (async () => {
    const un = await getCurrentWindow().listen<AutoSyncEvent>(
      AUTO_SYNC_EVENT,
      (event) => {
        if (!event.payload?.vaultId) return;
        useSyncStore.getState().applyAutoSyncEvent(event.payload);
      },
    );
    if (cancelled) un();
    else unlisten = un;
  })();
  return () => {
    cancelled = true;
    unlisten?.();
    unlisten = undefined;
  };
}
