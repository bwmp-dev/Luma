import { invoke } from "@tauri-apps/api/core";

export type McpStatus = {
  running: boolean;
  port: number | null;
  grantCount: number;
};

export type McpGrant = {
  id: string;
  label: string;
  /** Leading characters of the token. Never the token itself. */
  tokenPrefix: string;
  requireApproval: boolean;
  createdAt: number;
  lastUsedAt: number | null;
  hostIds: string[];
};

/**
 * The one and only time a token is available. Only its digest is stored, so
 * there is no way to show it again — the UI has to make that clear.
 */
export type McpGrantCreated = {
  grant: McpGrant;
  token: string;
};

export type McpSharedPane = {
  sessionId: string;
  title: string;
  kind: "ssh" | "local";
  hostId: string | null;
  grantId: string;
};

export type McpActivityEntry = {
  id: string;
  at: number;
  grantId: string;
  grantLabel: string;
  action: "run-command" | "pane-send";
  target: string;
  /** Size of what the agent sent. The content itself is never recorded. */
  inputBytes: number;
  exitCode: number | null;
  durationMs: number | null;
  error: string | null;
  /** Terminal session it ran in, or null for the non-interactive fallback. */
  sessionId: string | null;
  /** "tab" when it ran in a visible session, "exec" for a one-off connection. */
  via: "tab" | "exec" | null;
};

export type McpApprovalRequest = {
  id: string;
  grantId: string;
  grantLabel: string;
  action: "run-command" | "pane-send";
  target: string;
  preview: string;
};

/** Event carrying an approval request from the backend. */
export const MCP_APPROVAL_EVENT = "mcp-approval-request";

/**
 * The backend asking for a terminal session to run an agent's command in.
 *
 * Sessions are created frontend-first, so the backend cannot open one itself.
 * It asks, and waits for `mcpSessionReady`.
 */
export type McpSessionRequest = {
  id: string;
  grantId: string;
  grantLabel: string;
  hostId: string;
  hostName: string;
};

export const MCP_SESSION_EVENT = "mcp-session-request";

export function getMcpStatus(): Promise<McpStatus> {
  return invoke<McpStatus>("mcp_status");
}

export function listMcpGrants(): Promise<McpGrant[]> {
  return invoke<McpGrant[]>("mcp_grants_list");
}

export function createMcpGrant(
  label: string,
  hostIds: string[],
  requireApproval: boolean,
): Promise<McpGrantCreated> {
  return invoke<McpGrantCreated>("mcp_grant_create", {
    label,
    hostIds,
    requireApproval,
  });
}

export function updateMcpGrant(
  id: string,
  label: string,
  hostIds: string[],
  requireApproval: boolean,
): Promise<McpGrant> {
  return invoke<McpGrant>("mcp_grant_update", {
    id,
    label,
    hostIds,
    requireApproval,
  });
}

export function deleteMcpGrant(id: string): Promise<void> {
  return invoke<void>("mcp_grant_delete", { id });
}

export function listSharedPanes(): Promise<McpSharedPane[]> {
  return invoke<McpSharedPane[]>("mcp_shared_panes");
}

export function sharePane(
  sessionId: string,
  grantId: string,
  title: string,
): Promise<void> {
  return invoke<void>("mcp_pane_share", { sessionId, grantId, title });
}

export function unsharePane(sessionId: string): Promise<void> {
  return invoke<void>("mcp_pane_unshare", { sessionId });
}

export function resolveMcpApproval(
  requestId: string,
  allowed: boolean,
): Promise<void> {
  return invoke<void>("mcp_approval_resolve", { requestId, allowed });
}

/**
 * Answer a pending session request: the connected session to use, or why not.
 *
 * The backend re-validates the id and refuses one that is not a live session —
 * it is about to type a command into whatever this names.
 */
export function mcpSessionReady(
  requestId: string,
  session: { sessionId: string } | { error: string },
): Promise<void> {
  return invoke<void>("mcp_session_ready", {
    requestId,
    sessionId: "sessionId" in session ? session.sessionId : null,
    error: "error" in session ? session.error : null,
  });
}

export function listMcpActivity(): Promise<McpActivityEntry[]> {
  return invoke<McpActivityEntry[]>("mcp_activity_list");
}

export function getMcpExecutablePath(): Promise<string> {
  return invoke<string>("mcp_executable_path");
}

/**
 * The config block to paste into an MCP client.
 *
 * The bridge, not the client, attaches the token, which keeps it out of the
 * client's own header handling and out of shell history.
 */
export function buildClientConfig(executable: string, token: string): string {
  return JSON.stringify(
    {
      mcpServers: {
        luma: {
          command: executable,
          args: ["--mcp-stdio"],
          env: { LUMA_MCP_TOKEN: token },
        },
      },
    },
    null,
    2,
  );
}
