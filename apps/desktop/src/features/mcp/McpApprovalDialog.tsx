import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Modal } from "../../components/Modal";
import {
  MCP_APPROVAL_EVENT,
  resolveMcpApproval,
  type McpApprovalRequest,
} from "../../lib/mcp";

/**
 * Prompt shown when a grant with "ask before each action" wants to do
 * something. The agent's call blocks until this is answered.
 *
 * Requests queue rather than replacing one another: two agents, or one agent
 * running in parallel, must not silently lose a prompt. Dismissing without
 * choosing denies, matching the backend, which also denies on timeout.
 */
export function McpApprovalDialog() {
  const [queue, setQueue] = useState<McpApprovalRequest[]>([]);

  useEffect(() => {
    const unlisten = listen<McpApprovalRequest>(MCP_APPROVAL_EVENT, (event) => {
      setQueue((current) => [...current, event.payload]);
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, []);

  const current = queue[0];

  const answer = (allowed: boolean) => {
    if (!current) return;
    setQueue((rest) => rest.slice(1));
    void resolveMcpApproval(current.id, allowed).catch(() => {
      // Already resolved, or the request timed out and was denied. Either way
      // the prompt is gone and there is nothing to recover.
    });
  };

  if (!current) return null;

  const isCommand = current.action === "run-command";

  return (
    <Modal
      open
      onOpenChange={(open) => !open && answer(false)}
      title={isCommand ? "Run this command?" : "Send this input?"}
      description={`${current.grantLabel} wants to ${
        isCommand ? "run a command on" : "type into"
      } ${current.target}.`}
      footer={
        <>
          <button
            type="button"
            onClick={() => answer(false)}
            className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            Deny
          </button>
          <button
            type="button"
            autoFocus
            onClick={() => answer(true)}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground"
          >
            Allow once
          </button>
        </>
      }
    >
      <div className="space-y-2">
        <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-background px-3 py-2.5 font-mono text-xs text-foreground">
          {current.preview}
        </pre>
        <p className="text-xs text-muted">
          {queue.length > 1
            ? `${queue.length - 1} more waiting. `
            : ""}
          Unanswered prompts are denied after two minutes.
        </p>
      </div>
    </Modal>
  );
}
