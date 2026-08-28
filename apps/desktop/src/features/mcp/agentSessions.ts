import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  MCP_SESSION_EVENT,
  mcpSessionReady,
  type McpSessionRequest,
} from "../../lib/mcp";
import { parseLumaError } from "../../lib/hosts";
import { useSessionStore } from "../../stores/sessionStore";

async function openCommandSession(request: McpSessionRequest): Promise<void> {
  const sessionId = await useSessionStore
    .getState()
    .openSshSession(
      request.hostId,
      request.hostName,
      undefined,
      false,
      null,
      undefined,
      { background: true, mcpRequestId: request.id },
    );
  const session = useSessionStore
    .getState()
    .sessions.find((candidate) => candidate.id === sessionId);
  if (session?.status === "error") {
    throw new Error(session.errorMessage ?? "the connection failed");
  }
}

export function startAgentSessionListener(): () => void {
  let unlisten: (() => void) | undefined;
  let cancelled = false;
  void (async () => {
    const un = await getCurrentWindow().listen<McpSessionRequest>(
      MCP_SESSION_EVENT,
      (event) => {
        const request = event.payload;
        if (!request?.id || !request.hostId) return;
        void openCommandSession(request).catch(async (cause) => {
          const message =
            cause instanceof Error ? cause.message : parseLumaError(cause).message;
          console.error("[luma-mcp] could not open agent command tab", cause);
          await mcpSessionReady(
            request.id,
            `Luma could not open a terminal on ${request.hostName}: ${message}`,
          ).catch(() => undefined);
        });
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
