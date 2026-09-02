import { describe, it, expect, vi } from "vitest";
import { setInvoke } from "../../test/tauriMock";
import { createdTerminals, type Terminal } from "../../test/xtermMock";
import {
  terminalManager,
  isSpawnAbandoned,
  type SessionExit,
} from "./terminalManager";

/** No-op callback bundle satisfying the manager's SessionCallbacks. */
function callbacks(
  onExit: (exit: SessionExit) => void = () => {},
  onSshPrompt: (prompt: {
    type: "credential";
    label: string;
    target?: string;
    secret?: boolean;
  }) => void = () => {},
) {
  return {
    onTitle: () => {},
    onExit,
    onSearchRequested: () => {},
    onSshAuthenticated: () => {},
    onSshPrompt,
    onSshProgress: () => {},
    onRemoteOs: () => {},
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("terminalManager spawn races", () => {
  it("kills the backend that resolves after the session was disposed", async () => {
    const killed: string[] = [];
    let resolveSpawn: (() => void) | undefined;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        return new Promise((resolve) => {
          resolveSpawn = () =>
            resolve({ sessionId: "late-backend", shellName: "bash" });
        });
      }
      if (cmd === "pty_kill") {
        killed.push(args.sessionId as string);
        return undefined;
      }
      throw new Error(`unexpected ${cmd}`);
    });

    const promise = terminalManager.createSession(
      "disp-1",
      { kind: "local", ref: undefined },
      callbacks(),
    );
    // Dispose while the backend spawn is still in flight.
    terminalManager.dispose("disp-1");
    resolveSpawn?.();

    let disposeErr: unknown;
    await promise.catch((error: unknown) => {
      disposeErr = error;
    });
    expect(isSpawnAbandoned(disposeErr)).toBe(true);
    expect(killed).toContain("late-backend");
  });

  it("kills a superseded spawn when a restart happens mid-spawn", async () => {
    const killed: string[] = [];
    let resolveFirst: (() => void) | undefined;
    let firstStarted = false;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        if (!firstStarted) {
          firstStarted = true;
          return new Promise((resolve) => {
            resolveFirst = () =>
              resolve({ sessionId: "backend-old", shellName: "bash" });
          });
        }
        return { sessionId: "backend-new", shellName: "bash" };
      }
      if (cmd === "pty_kill") {
        killed.push(args.sessionId as string);
        return undefined;
      }
      throw new Error(`unexpected ${cmd}`);
    });

    const first = terminalManager.createSession(
      "restart-1",
      { kind: "local", ref: undefined },
      callbacks(),
    );
    // Restart before the first spawn resolves: this bumps the generation and
    // installs backend-new.
    const restarted = terminalManager.restart("restart-1");
    await restarted;
    resolveFirst?.();

    let staleErr: unknown;
    await first.catch((error: unknown) => {
      staleErr = error;
    });
    expect(isSpawnAbandoned(staleErr)).toBe(true);
    // The orphaned first backend must be killed; the winning one must not.
    expect(killed).toContain("backend-old");
    expect(killed).not.toContain("backend-new");

    terminalManager.dispose("restart-1");
  });

  it("does not resurrect a session whose restart spawn exits immediately", async () => {
    const exits: SessionExit[] = [];
    // First spawn stays alive; the restart spawn exits before its invoke resolves.
    let started = 0;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        started += 1;
        if (started === 2) {
          (args.onExit as { onmessage: (code: number | null) => void }).onmessage(
            0,
          );
        }
        return { sessionId: `backend-${started}`, shellName: "bash" };
      }
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    await terminalManager.createSession(
      "restart-2",
      { kind: "local", ref: undefined },
      callbacks((exit) => exits.push(exit)),
    );
    await terminalManager.restart("restart-2");
    await tick();

    // The restart's backend exited during spawn; exactly one exit reported.
    expect(exits).toHaveLength(1);
    expect(exits[0].code).toBe(0);

    terminalManager.dispose("restart-2");
  });
});

describe("terminalManager input flow", () => {
  it("serializes writes and coalesces input that arrives during IPC", async () => {
    const writes: string[] = [];
    const pendingResolvers: Array<() => void> = [];
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        return { sessionId: "input-backend", shellName: "bash" };
      }
      if (cmd === "pty_write") {
        writes.push(args.data as string);
        return new Promise<void>((resolve) => pendingResolvers.push(resolve));
      }
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    await terminalManager.createSession(
      "input-1",
      { kind: "local", ref: undefined },
      callbacks(),
    );

    terminalManager.sendInput("input-1", "a");
    terminalManager.sendInput("input-1", "b");
    terminalManager.sendInput("input-1", "\x7f");
    expect(writes).toEqual(["a"]);

    pendingResolvers.shift()?.();
    await tick();
    expect(writes).toEqual(["a", "b\x7f"]);

    pendingResolvers.shift()?.();
    await tick();
    terminalManager.dispose("input-1");
  });
});

describe("terminalManager SSH credential prompts", () => {
  it("parses split embedded prompt markers and deduplicates repeats", async () => {
    const prompts: Array<{
      type: "credential";
      label: string;
      target?: string;
      secret?: boolean;
    }> = [];
    let dataChannel:
      | { onmessage: (message: string | number[] | ArrayBuffer) => void }
      | undefined;
    setInvoke((cmd, args) => {
      if (cmd === "ssh_spawn") {
        dataChannel = args.onData as typeof dataChannel;
        return { sessionId: "prompt-backend", title: "jump host" };
      }
      if (cmd === "ssh_disconnect") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    await terminalManager.createSession(
      "prompt-1",
      { kind: "ssh", hostId: "host-1" },
      callbacks(() => {}, (prompt) => prompts.push(prompt)),
    );

    const payload = JSON.stringify({
      label: 'Verification code "OTP":',
      secret: false,
      target: "alice@jump.example.com",
    });
    const marker = `__LUMA_SSH_PROMPT__${payload}\r\n`;
    const split = marker.indexOf("secret");
    dataChannel?.onmessage(marker.slice(0, split));
    expect(prompts).toEqual([]);

    dataChannel?.onmessage(marker.slice(split));
    expect(prompts).toEqual([
      {
        type: "credential",
        label: 'Verification code "OTP":',
        secret: false,
        target: "alice@jump.example.com",
      },
    ]);

    dataChannel?.onmessage(marker);
    expect(prompts).toHaveLength(1);
    terminalManager.dispose("prompt-1");
  });
});

describe("terminalManager broadcast groups", () => {
  it("fans keystrokes out to every group member, once each, through the coalescing lane", async () => {
    const writes: Record<string, string[]> = {};
    let spawnCount = 0;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        spawnCount += 1;
        return { sessionId: `bc-backend-${spawnCount}`, shellName: "bash" };
      }
      if (cmd === "pty_write") {
        const id = args.sessionId as string;
        (writes[id] ??= []).push(args.data as string);
        return undefined;
      }
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    // Backend ids are assigned in creation order (bc-a -> 1, bc-b -> 2, bc-c -> 3).
    const startIndex = createdTerminals.length;
    await terminalManager.createSession("bc-a", { kind: "local", ref: undefined }, callbacks());
    await terminalManager.createSession("bc-b", { kind: "local", ref: undefined }, callbacks());
    await terminalManager.createSession("bc-c", { kind: "local", ref: undefined }, callbacks());
    const termA = createdTerminals[startIndex];
    const [backendA, backendB, backendC] = ["bc-backend-1", "bc-backend-2", "bc-backend-3"];

    // Group all three: typing into A fans the SAME byte out to B and C, and A
    // receives it exactly once (peers are the group minus self).
    terminalManager.setBroadcastGroup(["bc-a", "bc-b", "bc-c"]);
    termA.emitData("x");
    await tick();
    expect(writes[backendA]).toEqual(["x"]);
    expect(writes[backendB]).toEqual(["x"]);
    expect(writes[backendC]).toEqual(["x"]);

    // Exclude C (redefine the group without it): C stops receiving input.
    terminalManager.setBroadcastGroup(["bc-a", "bc-b"]);
    termA.emitData("y");
    await tick();
    expect(writes[backendA]).toEqual(["x", "y"]);
    expect(writes[backendB]).toEqual(["x", "y"]);
    expect(writes[backendC]).toEqual(["x"]); // unchanged

    // Disposing a member disbands a two-pane group; A then types only to itself.
    terminalManager.dispose("bc-b");
    termA.emitData("z");
    await tick();
    expect(writes[backendA]).toEqual(["x", "y", "z"]);
    expect(writes[backendB]).toEqual(["x", "y"]); // disposed, no new writes

    terminalManager.dispose("bc-a");
    terminalManager.dispose("bc-c");
  });

  it("stops fan-out once broadcast is disabled by clearing every former member", async () => {
    const writes: Record<string, string[]> = {};
    let spawnCount = 0;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        spawnCount += 1;
        return { sessionId: `off-backend-${spawnCount}`, shellName: "bash" };
      }
      if (cmd === "pty_write") {
        const id = args.sessionId as string;
        (writes[id] ??= []).push(args.data as string);
        return undefined;
      }
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    const startIndex = createdTerminals.length;
    await terminalManager.createSession("off-a", { kind: "local", ref: undefined }, callbacks());
    await terminalManager.createSession("off-b", { kind: "local", ref: undefined }, callbacks());
    const termA = createdTerminals[startIndex];
    const [backendA, backendB] = ["off-backend-1", "off-backend-2"];

    // Enable broadcast: typing into A fans out to B.
    terminalManager.setBroadcastGroup(["off-a", "off-b"]);
    termA.emitData("x");
    await tick();
    expect(writes[backendA]).toEqual(["x"]);
    expect(writes[backendB]).toEqual(["x"]);

    // Disable broadcast. The store computes an empty membership and, rather than
    // calling setBroadcastGroup([]) (which cannot find the shared peer set to
    // detach through an empty list), clears each former member individually so no
    // stale broadcastPeers set survives. Typing into A must no longer reach B.
    terminalManager.clearBroadcastGroup("off-a");
    terminalManager.clearBroadcastGroup("off-b");
    termA.emitData("y");
    await tick();
    expect(writes[backendA]).toEqual(["x", "y"]);
    expect(writes[backendB]).toEqual(["x"]); // unchanged: fan-out stopped

    terminalManager.dispose("off-a");
    terminalManager.dispose("off-b");
  });

  it("never delivers input to an excluded session even when it is the origin", async () => {
    const writes: Record<string, string[]> = {};
    let spawnCount = 0;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        spawnCount += 1;
        return { sessionId: `ex-backend-${spawnCount}`, shellName: "bash" };
      }
      if (cmd === "pty_write") {
        const id = args.sessionId as string;
        (writes[id] ??= []).push(args.data as string);
        return undefined;
      }
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });

    const startIndex = createdTerminals.length;
    await terminalManager.createSession("ex-a", { kind: "local", ref: undefined }, callbacks());
    await terminalManager.createSession("ex-b", { kind: "local", ref: undefined }, callbacks());
    const termB = createdTerminals[startIndex + 1];

    // Group only A; B is excluded. B still echoes its own keystrokes locally but
    // must not fan anything out (it has no peers) and A must not receive them.
    terminalManager.setBroadcastGroup(["ex-a"]); // fewer than two -> no group
    termB.emitData("q");
    await tick();
    expect(writes["ex-backend-1"]).toBeUndefined(); // A untouched
    expect(writes["ex-backend-2"]).toEqual(["q"]); // B typed to itself only

    terminalManager.dispose("ex-a");
    terminalManager.dispose("ex-b");
  });
});

describe("terminalManager shell integration", () => {
  /** Stub the PTY backend and create a local session, returning its terminal. */
  async function createLocal(id: string): Promise<Terminal> {
    setInvoke((cmd) => {
      if (cmd === "pty_spawn") return { sessionId: `${id}-backend`, shellName: "bash" };
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });
    const startIndex = createdTerminals.length;
    await terminalManager.createSession(id, { kind: "local", ref: undefined }, callbacks());
    return createdTerminals[startIndex];
  }

  it("records a command mark with exit code for an A/B/C/D sequence", async () => {
    const term = await createLocal("si-mark");
    term.markerLine = 0;
    term.emitOsc(133, "A"); // prompt start
    term.emitOsc(133, "B"); // command start (no state)
    term.markerLine = 1;
    term.emitOsc(133, "C"); // output start
    term.markerLine = 5;
    term.emitOsc(133, "D;1"); // finished, nonzero exit

    const marks = terminalManager.getCommandMarks("si-mark");
    expect(marks).toHaveLength(1);
    expect(marks[0].line).toBe(0);
    expect(marks[0].exitCode).toBe(1);
    expect(marks[0].failed).toBe(true);

    terminalManager.dispose("si-mark");
  });

  it("does not flag a successful command as failed", async () => {
    const term = await createLocal("si-ok");
    term.markerLine = 0;
    term.emitOsc(133, "A");
    term.markerLine = 1;
    term.emitOsc(133, "C");
    term.markerLine = 2;
    term.emitOsc(133, "D;0");

    const marks = terminalManager.getCommandMarks("si-ok");
    expect(marks).toHaveLength(1);
    expect(marks[0].exitCode).toBe(0);
    expect(marks[0].failed).toBe(false);

    terminalManager.dispose("si-ok");
  });

  it("caps the retained marks at 500", async () => {
    const term = await createLocal("si-cap");
    for (let i = 0; i < 600; i++) {
      term.markerLine = i;
      term.emitOsc(133, "A");
    }
    const marks = terminalManager.getCommandMarks("si-cap");
    expect(marks).toHaveLength(500);
    // The oldest were dropped: lines start at 100, end at 599.
    expect(marks[0].line).toBe(100);
    expect(marks[marks.length - 1].line).toBe(599);

    terminalManager.dispose("si-cap");
  });

  it("filters out marks whose marker was disposed by scrollback trim", async () => {
    const term = await createLocal("si-disp");
    term.markerLine = 0;
    term.emitOsc(133, "A");
    term.markerLine = 1;
    term.emitOsc(133, "A");
    term.markerLine = 2;
    term.emitOsc(133, "A");

    // xterm disposes markers when their line leaves the scrollback; simulate the
    // middle one being trimmed.
    term.markers[1].dispose();

    const marks = terminalManager.getCommandMarks("si-disp");
    expect(marks.map((mark) => mark.line)).toEqual([0, 2]);

    terminalManager.dispose("si-disp");
  });

  it("parses OSC 7 (Windows + POSIX) and OSC 1337 CurrentDir into getCwd", async () => {
    const term = await createLocal("si-cwd");
    expect(terminalManager.getCwd("si-cwd")).toBeNull();

    term.emitOsc(7, "file://myhost/C:/Users/me");
    expect(terminalManager.getCwd("si-cwd")).toBe("C:/Users/me");

    term.emitOsc(7, "file://myhost/home/me");
    expect(terminalManager.getCwd("si-cwd")).toBe("/home/me");

    term.emitOsc(1337, "CurrentDir=/var/log");
    expect(terminalManager.getCwd("si-cwd")).toBe("/var/log");

    // Non-CurrentDir OSC 1337 subcommands are ignored (cwd unchanged).
    term.emitOsc(1337, "SetMark");
    expect(terminalManager.getCwd("si-cwd")).toBe("/var/log");

    terminalManager.dispose("si-cwd");
  });

  it("copies the last command's output between the C and D marks", async () => {
    const writeText = vi.fn();
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    const term = await createLocal("si-copy");
    term.markerLine = 0;
    term.emitOsc(133, "A");
    term.markerLine = 1;
    term.emitOsc(133, "C"); // output starts on line 1
    term.setLine(1, "hello");
    term.setLine(2, "world");
    term.markerLine = 3;
    term.emitOsc(133, "D;0"); // output ends before line 3

    const copied = terminalManager.copyLastCommandOutput("si-copy");
    expect(copied).toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello\nworld");

    terminalManager.dispose("si-copy");
  });

  it("jumps the viewport to the previous/next prompt mark", async () => {
    const term = await createLocal("si-jump");
    term.markerLine = 2;
    term.emitOsc(133, "A");
    term.markerLine = 10;
    term.emitOsc(133, "A");

    term.viewportY = 20;
    terminalManager.jumpToPrompt("si-jump", "previous");
    expect(term.scrolledTo).toBe(10);

    term.viewportY = 5;
    terminalManager.jumpToPrompt("si-jump", "next");
    expect(term.scrolledTo).toBe(10);

    term.viewportY = 0;
    terminalManager.jumpToPrompt("si-jump", "previous"); // nothing before line 0
    expect(term.scrolledTo).toBe(10); // unchanged

    terminalManager.dispose("si-jump");
  });

  it("degrades gracefully with no marks (actions are no-ops)", async () => {
    const term = await createLocal("si-none");
    expect(terminalManager.hasCommandMarks("si-none")).toBe(false);
    expect(terminalManager.getCwd("si-none")).toBeNull();
    expect(terminalManager.copyLastCommandOutput("si-none")).toBe(false);
    expect(terminalManager.copyCwd("si-none")).toBe(false);
    terminalManager.jumpToPrompt("si-none", "next");
    expect(term.scrolledTo).toBeNull();

    terminalManager.dispose("si-none");
  });
});

describe("terminalManager fit overflow", () => {
  /** Mount a session into a host of `hostHeight` px whose rendered grid measures
   * `renderedHeight` px, then fit. jsdom does no layout, so both heights are
   * stubbed: clientHeight on the host and the screen's bounding rect. */
  async function fitInto(
    id: string,
    hostHeight: number,
    renderedHeight: number,
    hostPaddingTop = 0,
  ): Promise<Terminal> {
    setInvoke((cmd) => {
      if (cmd === "pty_spawn") return { sessionId: `${id}-backend`, shellName: "bash" };
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });
    const startIndex = createdTerminals.length;
    await terminalManager.createSession(id, { kind: "local", ref: undefined }, callbacks());
    const term = createdTerminals[startIndex];

    const host = document.createElement("div");
    // The pane host reserves top padding (pl-2 pt-1.5); clientHeight includes it.
    if (hostPaddingTop) host.style.paddingTop = `${hostPaddingTop}px`;
    document.body.appendChild(host);
    Object.defineProperty(host, "clientHeight", {
      value: hostHeight,
      configurable: true,
    });
    terminalManager.attach(id, host);

    const screen = term.element?.querySelector(".xterm-screen") as HTMLElement;
    screen.getBoundingClientRect = () =>
      ({ height: renderedHeight }) as DOMRect;
    return term;
  }

  it("drops a row when the fitted grid renders past the container", async () => {
    // 24 rows measuring 3px taller than the space available clips the last line.
    const term = await fitInto("fit-over", 500, 503);
    term.resize(80, 24);

    terminalManager.fitSession("fit-over");

    expect(term.rows).toBe(23);
    expect(term.cols).toBe(80);

    terminalManager.dispose("fit-over");
  });

  it("keeps the grid when it fits, ignoring sub-pixel overflow", async () => {
    const term = await fitInto("fit-exact", 500, 500.4);
    term.resize(80, 24);

    terminalManager.fitSession("fit-exact");

    expect(term.rows).toBe(24);

    terminalManager.dispose("fit-exact");
  });

  it("drops a row when the grid overflows the host's padded content box", async () => {
    // clientHeight (500) counts the host's 6px top padding, so a grid rendering
    // 498px sits within clientHeight yet overflows the 494px content box the
    // terminal actually fills — its last line is clipped unless a row is dropped.
    const term = await fitInto("fit-pad", 500, 498, 6);
    term.resize(80, 24);

    terminalManager.fitSession("fit-pad");

    expect(term.rows).toBe(23);

    terminalManager.dispose("fit-pad");
  });

  it("never drops below a single row", async () => {
    const term = await fitInto("fit-tiny", 10, 40);
    term.resize(80, 1);

    terminalManager.fitSession("fit-tiny");

    expect(term.rows).toBe(1);

    terminalManager.dispose("fit-tiny");
  });
});

describe("terminalManager buffer snapshot", () => {
  /** Stub the PTY backend and create a local session, returning its terminal. */
  async function createLocal(id: string): Promise<Terminal> {
    setInvoke((cmd) => {
      if (cmd === "pty_spawn") return { sessionId: `${id}-backend`, shellName: "bash" };
      if (cmd === "pty_kill") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });
    const startIndex = createdTerminals.length;
    await terminalManager.createSession(id, { kind: "local", ref: undefined }, callbacks());
    return createdTerminals[startIndex];
  }

  it("serializes plain lines without escapes and drops trailing blanks", async () => {
    const term = await createLocal("buf-plain");
    term.setLine(0, "hello   ");
    term.setLine(1, "world");

    // Trailing unstyled blanks are dropped, and so are the empty viewport rows
    // below the last content line.
    expect(terminalManager.getBufferText("buf-plain")).toBe("hello\r\nworld");

    terminalManager.dispose("buf-plain");
  });

  it("emits SGR escapes so a replayed buffer keeps its colors", async () => {
    const term = await createLocal("buf-color");
    // "ab" red-on-default, "cd" plain.
    term.setLine(0, "abcd", [
      { fgPalette: 1 },
      { fgPalette: 1 },
      undefined,
      undefined,
    ]);

    // Leaving a styled run resets so attributes never leak into the next one.
    expect(terminalManager.getBufferText("buf-color")).toBe(
      "\x1b[38;5;1mab\x1b[0mcd",
    );

    terminalManager.dispose("buf-color");
  });

  it("unstyles interior blanks that separate styled runs", async () => {
    const term = await createLocal("buf-gap");
    term.setLine(0, "a b", [{ bgPalette: 4 }, undefined, { bgPalette: 4 }]);

    // The gap must not inherit the run's background, or the replayed line shows
    // a solid block where the original had none.
    expect(terminalManager.getBufferText("buf-gap")).toBe(
      "\x1b[48;5;4ma\x1b[0m \x1b[48;5;4mb\x1b[0m",
    );

    terminalManager.dispose("buf-gap");
  });

  it("clears a display session so re-applying a snapshot is idempotent", () => {
    const startIndex = createdTerminals.length;
    terminalManager.createDisplaySession("disp-reset");
    const term = createdTerminals[startIndex];
    const reset = vi.spyOn(term, "reset");
    const write = vi.spyOn(term, "write");

    terminalManager.resetOutput("disp-reset");
    terminalManager.writeOutput("disp-reset", "snapshot");

    expect(reset).toHaveBeenCalled();
    expect(write).toHaveBeenCalledWith("snapshot");

    terminalManager.dispose("disp-reset");
  });

  it("does not reset a backend session (its output comes from the PTY)", async () => {
    const term = await createLocal("buf-backend");
    const reset = vi.spyOn(term, "reset");

    terminalManager.resetOutput("buf-backend");

    expect(reset).not.toHaveBeenCalled();

    terminalManager.dispose("buf-backend");
  });
});

describe("terminalManager session previews", () => {
  /** Stub the PTY backend, create a local session, and return its terminal plus
   * the data channel the backend would push output through. */
  async function createLocalWithOutput(id: string): Promise<{
    term: Terminal;
    emit: (data: string) => void;
  }> {
    let dataChannel: { onmessage: (message: string) => void } | undefined;
    setInvoke((cmd, args) => {
      if (cmd === "pty_spawn") {
        dataChannel = args.onData as typeof dataChannel;
        return { sessionId: `${id}-backend`, shellName: "bash" };
      }
      if (cmd === "pty_kill" || cmd === "pty_resize") return undefined;
      throw new Error(`unexpected ${cmd}`);
    });
    const startIndex = createdTerminals.length;
    await terminalManager.createSession(id, { kind: "local", ref: undefined }, callbacks());
    return {
      term: createdTerminals[startIndex],
      emit: (data) => dataChannel?.onmessage(data),
    };
  }

  /** A preview card of the given pixel box. jsdom performs no layout, so the box
   * is stubbed the way the fit-overflow suite does it. */
  function card(size: { width: number; height: number }): HTMLElement {
    const host = document.createElement("div");
    document.body.appendChild(host);
    Object.defineProperty(host, "clientWidth", {
      value: size.width,
      configurable: true,
    });
    Object.defineProperty(host, "clientHeight", {
      value: size.height,
      configurable: true,
    });
    return host;
  }

  /**
   * Report `grid` as the pixel size the session's terminal renders at. A preview
   * is measured THROUGH its transform, so — like a browser — the stub multiplies
   * by whatever scale is currently applied.
   */
  function stubRendering(
    sessionId: string,
    term: Terminal,
    grid: { width: number; height: number },
  ): void {
    const screen = term.element?.querySelector(".xterm-screen") as HTMLElement;
    screen.getBoundingClientRect = () => {
      const scale = terminalManager.previewScale(sessionId) ?? 1;
      return { width: grid.width * scale, height: grid.height * scale } as DOMRect;
    };
  }

  /**
   * Park a live session in a card and report the rendering it measures.
   *
   * The session is given a written last row first: a card is anchored on the
   * output, not on the grid, so a terminal with an empty buffer would legitimately
   * report nothing to scroll to and every geometry assertion below would be about
   * a blank screen rather than about the fit.
   */
  async function previewInto(
    id: string,
    host: { width: number; height: number },
    grid: { width: number; height: number },
    onChange?: () => void,
  ): Promise<{
    term: Terminal;
    host: HTMLElement;
    release: () => void;
    emit: (data: string) => void;
  }> {
    const { term, emit } = await createLocalWithOutput(id);
    term.setLine(term.rows - 1, "$ the last line of output");
    const element = card(host);
    const release = terminalManager.previewSession(id, element, { onChange });
    stubRendering(id, term, grid);
    return { term, host: element, release, emit };
  }

  /** The `transform` currently drawing a previewed session into its card. */
  function transformOf(term: Terminal): string {
    return term.element?.style.transform ?? "";
  }

  it("shows the session's own terminal rather than building a second one", async () => {
    const { term, emit } = await createLocalWithOutput("preview-same");
    const before = createdTerminals.length;
    const host = card({ width: 300, height: 150 });

    const release = terminalManager.previewSession("preview-same", host);

    // No mirror was constructed, and the element in the card is the session's.
    expect(createdTerminals.length).toBe(before);
    expect(host.contains(term.element as HTMLElement)).toBe(true);
    // So live output needs no forwarding: it lands in the terminal the card
    // holds because that is the terminal it was always going to land in.
    emit("live output");
    expect(term.writes).toEqual(["live output"]);

    release();
    host.remove();
    terminalManager.dispose("preview-same");
  });

  it("makes a parked session read-only, and gives its input back on release", async () => {
    const { term } = await createLocalWithOutput("preview-ro");
    const host = card({ width: 300, height: 150 });

    const release = terminalManager.previewSession("preview-ro", host);
    // A card is decorative: nothing tapped or typed on one may reach the PTY.
    expect(term.options.disableStdin).toBe(true);

    release();
    expect(term.options.disableStdin).toBe(false);

    host.remove();
    terminalManager.dispose("preview-ro");
  });

  it("hands the element back on release", async () => {
    const { term, host, release } = await previewInto(
      "preview-release",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );
    terminalManager.fitPreview("preview-release");
    expect(transformOf(term)).not.toBe("");

    release();

    // Detached and unstyled: the full-screen pane that claims it next attaches a
    // terminal with the geometry it expects, not one still scaled into a card.
    expect(host.contains(term.element as HTMLElement)).toBe(false);
    expect(transformOf(term)).toBe("");
    expect(term.element?.style.position).toBe("");
    expect(terminalManager.previewScale("preview-release")).toBeNull();

    host.remove();
    terminalManager.dispose("preview-release");
  });

  it("no-ops for a session that does not exist yet", async () => {
    const before = createdTerminals.length;
    const host = card({ width: 300, height: 150 });

    const release = terminalManager.previewSession("preview-absent", host);
    release();

    expect(createdTerminals.length).toBe(before);
    // Crucially the card was not parked for a later session to claim: a session
    // that attached itself to a card on creation would fit its shell to it.
    await createLocalWithOutput("preview-absent");
    expect(host.childElementCount).toBe(0);
    expect(terminalManager.previewScale("preview-absent")).toBeNull();

    host.remove();
    terminalManager.dispose("preview-absent");
  });

  it("does not lease the same session to a second card", async () => {
    const { term } = await createLocalWithOutput("preview-dupe");
    const first = card({ width: 300, height: 150 });
    const second = card({ width: 300, height: 150 });

    const release = terminalManager.previewSession("preview-dupe", first);
    const duplicate = terminalManager.previewSession("preview-dupe", second);
    duplicate();

    // The duplicate took nothing and its release gave nothing back: the element
    // is still in the first card, still read-only.
    expect(first.contains(term.element as HTMLElement)).toBe(true);
    expect(term.options.disableStdin).toBe(true);

    release();
    first.remove();
    second.remove();
    terminalManager.dispose("preview-dupe");
  });

  it("refuses to refit a parked session, so a card cannot resize the shell", async () => {
    const { term, host, release } = await previewInto(
      "preview-nofit",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );
    term.resize(140, 40);

    // The card is nothing like the screen the shell is sized for; fitting the
    // grid to it would re-wrap the output and resize the PTY.
    terminalManager.fitSession("preview-nofit");
    terminalManager.fitPreview("preview-nofit");

    expect(term.cols).toBe(140);
    expect(term.rows).toBe(40);

    release();
    host.remove();
    terminalManager.dispose("preview-nofit");
  });

  it("scales the rendering until the terminal's width fits the card", async () => {
    // A rendering twice as wide as the card is drawn at half size. Neither the
    // grid nor the font it renders at is touched, so the card holds the
    // terminal's own output, shrunk.
    const { term, host, release } = await previewInto(
      "fit-preview",
      { width: 300, height: 400 },
      { width: 600, height: 400 },
    );

    expect(terminalManager.fitPreview("fit-preview")).toBe(0.5);
    expect(term.options.fontSize).toBe(14);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);

    release();
    host.remove();
    terminalManager.dispose("fit-preview");
  });

  it("anchors the last written row, so the card shows the most recent output", async () => {
    // 900px of output at half scale is 450 on screen, in a 150px card: the last
    // 150px — the newest lines — are what the card is read for, so the element
    // is slid up by the 300px above them rather than shrunk until it all fit.
    const { term, host, release } = await previewInto(
      "fit-preview-tail",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );

    expect(terminalManager.fitPreview("fit-preview-tail")).toBe(0.5);
    expect(transformOf(term)).toBe("translateY(-300px) scale(0.5)");
    // Positioned out of flow: a grid three times the card's height must not
    // stretch the card that clips it.
    expect(term.element?.style.position).toBe("absolute");

    release();
    host.remove();
    terminalManager.dispose("fit-preview-tail");
  });

  it("leaves output that fits at the top of the card", async () => {
    const { term, host, release } = await previewInto(
      "fit-preview-short",
      { width: 300, height: 400 },
      { width: 600, height: 400 },
    );

    // 400px of output at half scale is 200 in a 400px card; sliding it down to
    // the bottom would open a gap above the first row.
    terminalManager.fitPreview("fit-preview-short");
    expect(transformOf(term)).toBe("translateY(0px) scale(0.5)");

    release();
    host.remove();
    terminalManager.dispose("fit-preview-short");
  });

  it("anchors the output, not the grid, when the session has not filled its screen", async () => {
    // A shell that has printed six lines into a 24-row screen leaves 18 empty.
    // Anchoring on the grid would faithfully photograph that emptiness — the
    // card would be blank while the session plainly is not.
    const { term, host, release } = await previewInto(
      "fit-preview-unfilled",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );
    term.lines.clear();
    for (let row = 0; row < 6; row += 1) term.setLine(row, `line ${row}`);

    terminalManager.fitPreview("fit-preview-unfilled");

    // Six of 24 rows over 900px is 225px of output; at half scale that is 112.5
    // in a 150px card, so it all fits and sits at the top.
    expect(transformOf(term)).toBe("translateY(0px) scale(0.5)");

    release();
    host.remove();
    terminalManager.dispose("fit-preview-unfilled");
  });

  it("keeps the prompt in view on a row that is still blank", async () => {
    const { term, host, release } = await previewInto(
      "fit-preview-cursor",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );
    term.lines.clear();
    // Nothing written yet on the row the cursor sits on: the reader is looking
    // at a prompt, which the card must not crop off as empty.
    term.cursorY = 11;

    terminalManager.fitPreview("fit-preview-cursor");

    // 12 of 24 rows over 900px is 450px; at half scale, 225 in a 150px card.
    expect(transformOf(term)).toBe("translateY(-75px) scale(0.5)");

    release();
    host.remove();
    terminalManager.dispose("fit-preview-cursor");
  });

  it("re-anchors as the session writes, without the card changing size", async () => {
    const fits: number[] = [];
    const { host, release, emit } = await previewInto(
      "fit-preview-live",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
      () => fits.push(1),
    );
    fits.length = 0;

    emit("a new line of output\r\n");

    // Nothing about the card changed, so a ResizeObserver on it would see
    // nothing — but the row the card is anchored on just moved.
    expect(fits).toHaveLength(1);

    release();
    emit("more output nobody is showing");
    // The subscription goes with the lease: a released card is not still being
    // asked to refit a terminal it no longer holds.
    expect(fits).toHaveLength(1);

    host.remove();
    terminalManager.dispose("fit-preview-live");
  });

  it("crops to whole rows, never to a band of half-drawn glyphs", async () => {
    // 900px of output at half scale is 450 in a 100px card, so 350px has to go —
    // 18.67 rows of 18.75. Sliding by exactly 350 would leave two thirds of a row
    // of clipped text along the top edge; the whole row goes instead.
    const { term, host, release } = await previewInto(
      "fit-preview-rowsnap",
      { width: 300, height: 100 },
      { width: 600, height: 900 },
    );

    terminalManager.fitPreview("fit-preview-rowsnap");

    expect(transformOf(term)).toBe("translateY(-356.25px) scale(0.5)");

    release();
    host.remove();
    terminalManager.dispose("fit-preview-rowsnap");
  });

  it("never draws a preview larger than the terminal itself", async () => {
    // A rendering far narrower than the card could scale up 4x; the card shows
    // the terminal, so it stops at the terminal's own size.
    const { host, release } = await previewInto(
      "fit-preview-cap",
      { width: 400, height: 400 },
      { width: 100, height: 100 },
    );

    expect(terminalManager.fitPreview("fit-preview-cap")).toBe(1);

    release();
    host.remove();
    terminalManager.dispose("fit-preview-cap");
  });

  it("clamps at the readable floor rather than shrinking to a smear", async () => {
    const { host, release } = await previewInto(
      "fit-preview-floor",
      { width: 20, height: 400 },
      { width: 2000, height: 400 },
    );

    // 20/2000 would be 0.01; the wide grid is clipped at the right edge instead,
    // keeping the start of each line — where the prompt and command live.
    expect(terminalManager.fitPreview("fit-preview-floor")).toBe(0.35);

    release();
    host.remove();
    terminalManager.dispose("fit-preview-floor");
  });

  it("reports the scale unchanged once it fits, so a caller stops iterating", async () => {
    const { host, release } = await previewInto(
      "fit-preview-settled",
      { width: 400, height: 400 },
      { width: 400, height: 400 },
    );

    expect(terminalManager.fitPreview("fit-preview-settled")).toBe(1);
    expect(terminalManager.previewScale("fit-preview-settled")).toBe(1);
    expect(terminalManager.fitPreview("fit-preview-settled")).toBe(1);

    release();
    host.remove();
    terminalManager.dispose("fit-preview-settled");
  });

  it("asks the card to refit when Appearance changes the font", async () => {
    const changes: number[] = [];
    const { term, host, release } = await previewInto(
      "fit-preview-restyle",
      { width: 300, height: 400 },
      { width: 600, height: 400 },
      () => changes.push(1),
    );
    changes.length = 0;

    terminalManager.applyTerminalStyle({ fontSize: 20 });

    // The session is real, so it takes the new size like any other; its card has
    // to refit because the rendering it scales just changed shape underneath it.
    expect(term.options.fontSize).toBe(20);
    expect(changes.length).toBeGreaterThan(0);

    release();
    host.remove();
    terminalManager.dispose("fit-preview-restyle");
    terminalManager.applyTerminalStyle({ fontSize: 14 });
  });

  it("does nothing for a session that is not in a card", async () => {
    await createLocalWithOutput("fit-preview-real");

    expect(terminalManager.fitPreview("fit-preview-real")).toBeNull();
    expect(terminalManager.previewScale("fit-preview-real")).toBeNull();

    terminalManager.dispose("fit-preview-real");
  });

  it("survives the session being closed while its card still holds it", async () => {
    const { host, release } = await previewInto(
      "preview-closed",
      { width: 300, height: 150 },
      { width: 600, height: 900 },
    );

    // Closing a session from the card's own context menu disposes it before the
    // list has unmounted the card.
    terminalManager.dispose("preview-closed");

    expect(() => release()).not.toThrow();
    expect(terminalManager.fitPreview("preview-closed")).toBeNull();

    host.remove();
  });
});
