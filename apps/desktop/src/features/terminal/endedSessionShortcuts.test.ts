import { describe, it, expect } from "vitest";
import { endedSessionAction } from "./endedSessionShortcuts";
import type { TerminalSession } from "../../types";

function session(overrides: Partial<TerminalSession> = {}): TerminalSession {
  return {
    id: "s1",
    title: "host",
    type: "ssh",
    status: "disconnected",
    activePaneId: "p1",
    ...overrides,
  };
}

const ctrl = (code: string) => ({
  ctrlKey: true,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  code,
});

describe("endedSessionAction", () => {
  it("reconnects on Ctrl+R and closes on Ctrl+D once the session ended", () => {
    expect(endedSessionAction(session(), ctrl("KeyR"))).toBe("reconnect");
    expect(endedSessionAction(session(), ctrl("KeyD"))).toBe("close");
    expect(endedSessionAction(session({ status: "error" }), ctrl("KeyD"))).toBe("close");
  });

  it("leaves live and connecting sessions to the shell", () => {
    for (const status of ["connected", "connecting"] as const) {
      expect(endedSessionAction(session({ status }), ctrl("KeyR"))).toBeNull();
      expect(endedSessionAction(session({ status }), ctrl("KeyD"))).toBeNull();
    }
  });

  it("ignores sessions waiting on an auto-reconnect", () => {
    const waiting = session({ status: "error", connectionState: "reconnecting" });
    expect(endedSessionAction(waiting, ctrl("KeyD"))).toBeNull();
  });

  it("requires plain Ctrl", () => {
    expect(endedSessionAction(session(), { ...ctrl("KeyD"), shiftKey: true })).toBeNull();
    expect(endedSessionAction(session(), { ...ctrl("KeyR"), metaKey: true })).toBeNull();
    expect(endedSessionAction(session(), { ...ctrl("KeyR"), ctrlKey: false })).toBeNull();
  });

  it("does not reconnect where the pane offers no retry", () => {
    const changed = session({ status: "error", errorCategory: "host-key-changed" });
    expect(endedSessionAction(changed, ctrl("KeyR"))).toBeNull();
    expect(endedSessionAction(changed, ctrl("KeyD"))).toBe("close");
    const agent = session({ agentCommand: true });
    expect(endedSessionAction(agent, ctrl("KeyR"))).toBeNull();
  });
});
