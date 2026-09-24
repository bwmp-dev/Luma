import type { TerminalSession } from "../../types";

export type EndedSessionAction = "reconnect" | "close";

type KeyEventLike = {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  code: string;
};

/** A session whose backend has exited or failed and is not waiting on an
 * auto-reconnect: the pane shows the disconnect banner or an error card, and
 * keystrokes no longer reach a shell. */
export function isEndedSession(session: TerminalSession): boolean {
  return (
    (session.status === "disconnected" || session.status === "error") &&
    session.connectionState !== "reconnecting"
  );
}

/** Whether the pane offers a reconnect/restart for this ended session. Mirrors
 * PaneView: agent-run commands are one-shot, and a changed host key must be
 * resolved in Known Hosts rather than retried. */
export function canRestartEndedSession(session: TerminalSession): boolean {
  return !session.agentCommand && session.errorCategory !== "host-key-changed";
}

/**
 * Ctrl+R reconnects and Ctrl+D closes an ended session. Ctrl+D is EOF, so a
 * second press after the shell exits closes the pane just like a terminal
 * emulator closing on exit. Only ended sessions claim these chords, so a live
 * shell keeps reverse-search and EOF.
 */
export function endedSessionAction(
  session: TerminalSession,
  event: KeyEventLike,
): EndedSessionAction | null {
  if (!isEndedSession(session)) return null;
  if (!event.ctrlKey || event.altKey || event.shiftKey || event.metaKey) return null;
  if (event.code === "KeyD") return "close";
  if (event.code === "KeyR" && canRestartEndedSession(session)) return "reconnect";
  return null;
}
