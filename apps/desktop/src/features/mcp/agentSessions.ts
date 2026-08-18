import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  MCP_SESSION_EVENT,
  mcpSessionReady,
  type McpSessionRequest,
} from "../../lib/mcp";
import { parseLumaError } from "../../lib/hosts";
import { useSessionStore } from "../../stores/sessionStore";
import { terminalManager } from "../terminal/terminalManager";
import type { TerminalSession } from "../../types";

/*
 * Opening a terminal tab on an agent's behalf.
 *
 * Sessions are created frontend-first — the store builds the tab, xterm attaches
 * and sizes it, and only then does it invoke the spawn — so the backend cannot
 * open one itself. It emits `mcp-session-request` and waits here.
 *
 * The tab opens in the background: an agent acting is worth seeing, not worth
 * interrupting whatever the user is doing. Once it exists the command is typed
 * into a real session, which is the point — a sudo prompt renders in a terminal
 * the user can actually answer.
 */

/** How long to wait for a session to finish connecting. Covers a host-key
 * prompt and a credential prompt, both of which wait on the user. The backend
 * applies its own (shorter) deadline, so this is only a backstop against a
 * session that never resolves either way. */
const CONNECT_TIMEOUT_MS = 120_000;

/** Sessions opened for an agent, keyed by `grantId::hostId` so a second command
 * on the same host reuses the same tab (and therefore its shell state). */
const agentSessions = new Map<string, string>();

function key(grantId: string, hostId: string): string {
  return `${grantId}::${hostId}`;
}

function findSession(sessionId: string): TerminalSession | undefined {
  return useSessionStore.getState().sessions.find((s) => s.id === sessionId);
}

/** A session that is connected and still belongs to the host we want. */
function isUsable(session: TerminalSession | undefined, hostId: string): boolean {
  return (
    session !== undefined &&
    session.type === "ssh" &&
    session.hostId === hostId &&
    session.status === "connected"
  );
}

/**
 * Wait for a freshly opened session to reach a terminal state.
 *
 * Connecting involves prompts the user answers, so this watches the store
 * rather than assuming the spawn call resolving means "ready".
 */
function waitForConnection(sessionId: string, hostId: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const settle = (error?: string) => {
      window.clearTimeout(timer);
      unsubscribe();
      if (error) reject(new Error(error));
      else resolve();
    };

    const check = () => {
      const session = findSession(sessionId);
      if (!session) {
        settle("the terminal was closed before the command could run");
        return;
      }
      if (isUsable(session, hostId)) {
        settle();
        return;
      }
      if (session.status === "error" || session.status === "disconnected") {
        settle(session.errorMessage ?? "the connection failed");
      }
    };

    const timer = window.setTimeout(
      () => settle("the connection timed out"),
      CONNECT_TIMEOUT_MS,
    );
    const unsubscribe = useSessionStore.subscribe(check);
    // The session may already be connected — a reused host key and stored
    // credentials can settle it before the first subscription fires.
    check();
  });
}

/**
 * The id the backend knows this session by.
 *
 * The store's session id and the backend's are different UUIDs — `ssh_spawn`
 * mints its own and returns it, and that is what the tap registry and
 * `is_authenticated` are keyed by. Reporting the store id would name a session
 * the backend has never heard of. It also changes on every restart, so it is
 * resolved at use rather than cached.
 */
function backendIdOf(sessionId: string): string {
  const backendId = terminalManager.getBackendId(sessionId);
  if (!backendId) throw new Error("the terminal is not connected");
  return backendId;
}

/** Resolve one request: reuse a live agent session, or open a new one.
 *
 * Returns the BACKEND session id, which is what the caller must report. */
async function provideSession(request: McpSessionRequest): Promise<string> {
  const existing = agentSessions.get(key(request.grantId, request.hostId));
  if (existing && isUsable(findSession(existing), request.hostId)) {
    const backendId = terminalManager.getBackendId(existing);
    // A session that lost its backend (restarting) is not reusable; fall
    // through and open a fresh one rather than naming a dead id.
    if (backendId) return backendId;
  }

  const sessionId = await useSessionStore
    .getState()
    .openSshSession(
      request.hostId,
      request.hostName,
      undefined,
      false,
      null,
      undefined,
      { background: true },
    );
  await waitForConnection(sessionId, request.hostId);
  agentSessions.set(key(request.grantId, request.hostId), sessionId);
  return backendIdOf(sessionId);
}

/**
 * Subscribe once to backend session requests. Wired from app bootstrap exactly
 * like `startAgentInboxListener`. Returns an unlisten cleanup.
 */
export function startAgentSessionListener(): () => void {
  let unlisten: (() => void) | undefined;
  let cancelled = false;
  void (async () => {
    const un = await getCurrentWindow().listen<McpSessionRequest>(
      MCP_SESSION_EVENT,
      (event) => {
        const request = event.payload;
        if (!request?.id || !request.hostId) return;
        void (async () => {
          try {
            const sessionId = await provideSession(request);
            await mcpSessionReady(request.id, { sessionId });
          } catch (cause) {
            // The agent gets the reason rather than a silent timeout — "host key
            // refused" is something it can report to the user and act on.
            // Backend rejections arrive as {category, message} rather than an
            // Error, and String()-ing one yields "[object Object]".
            const message =
              cause instanceof Error
                ? cause.message
                : parseLumaError(cause).message;
            console.error("[luma-mcp] could not open agent session", cause);
            await mcpSessionReady(request.id, {
              error: `Luma could not open a terminal on ${request.hostName}: ${message}`,
            }).catch(() => undefined);
          }
        })();
      },
    );
    if (cancelled) un();
    else unlisten = un;
  })();
  return () => {
    cancelled = true;
    unlisten?.();
    unlisten = undefined;
    agentSessions.clear();
  };
}
