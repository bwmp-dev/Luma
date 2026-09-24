import type { TerminalSession } from "../../types";

export type EndedSessionAction = "reconnect" | "close";

type KeyEventLike = {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  code: string;
};

export function isEndedSession(session: TerminalSession): boolean {
  return (
    (session.status === "disconnected" || session.status === "error") &&
    session.connectionState !== "reconnecting"
  );
}

// Mirrors when PaneView offers a Reconnect/Retry button.
export function canRestartEndedSession(session: TerminalSession): boolean {
  return !session.agentCommand && session.errorCategory !== "host-key-changed";
}

// Only ended sessions claim these, so a live shell keeps reverse-search and EOF.
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
