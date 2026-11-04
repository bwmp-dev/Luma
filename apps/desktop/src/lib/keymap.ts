/*
 * Central keymap registry: the single source of truth for Luma's rebindable
 * GLOBAL keyboard shortcuts. Everything here is pure (no React, no Tauri) so it
 * can be unit-tested and reused by both the window key handler (Layout) and the
 * terminal's custom key handler (terminalManager, which swallows app chords so
 * they bubble up to the window listener).
 *
 * Chord format: modifier tokens (Ctrl/Alt/Shift/Meta) + a key token, joined with
 * "+", e.g. "Ctrl+Shift+B" or "Ctrl+Alt+Up". Matching is done against a
 * KeyboardEvent using `event.code` (so letters/digits are layout-independent,
 * matching the existing terminal key-handler checks). Modifier equality is exact
 * so "Ctrl+Shift+T" never fires for "Ctrl+Shift+Alt+T".
 *
 * NOTE ON macOS: default chords use literal "Ctrl". On macOS the primary
 * accelerator is normally Cmd (Meta); users can rebind to Meta explicitly in the
 * settings editor. The dev/target platform is Windows, where Ctrl is correct.
 */

export type KeymapGroup = "Workspace" | "Terminal";

export type KeymapActionId =
  | "workspace.newTab"
  | "workspace.splitRight"
  | "workspace.splitDown"
  | "workspace.closePane"
  | "workspace.commandPalette"
  | "workspace.toggleBroadcast"
  | "workspace.goToTab1"
  | "workspace.goToTab2"
  | "workspace.goToTab3"
  | "workspace.goToTab4"
  | "workspace.goToTab5"
  | "workspace.goToTab6"
  | "workspace.goToTab7"
  | "workspace.goToTab8"
  | "workspace.goToLastTab"
  | "workspace.focusPaneLeft"
  | "workspace.focusPaneRight"
  | "workspace.focusPaneUp"
  | "workspace.focusPaneDown"
  | "terminal.jumpPreviousPrompt"
  | "terminal.jumpNextPrompt";

export type KeymapActionDef = {
  id: KeymapActionId;
  label: string;
  group: KeymapGroup;
  defaultChord: string;
};

const TAB_NUMBERS = [1, 2, 3, 4, 5, 6, 7, 8] as const;

/** Ordered registry of every rebindable global action. Order drives both the
 * settings editor listing and resolveAction()'s first-match precedence. */
export const KEYMAP_ACTIONS: readonly KeymapActionDef[] = [
  { id: "workspace.newTab", label: "New tab", group: "Workspace", defaultChord: "Ctrl+Shift+T" },
  {
    id: "workspace.splitRight",
    label: "Split right",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+D",
  },
  {
    id: "workspace.splitDown",
    label: "Split down",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+E",
  },
  {
    id: "workspace.closePane",
    label: "Close pane",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+W",
  },
  {
    id: "workspace.commandPalette",
    label: "Command palette",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+P",
  },
  {
    id: "workspace.toggleBroadcast",
    label: "Toggle broadcast input",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+B",
  },
  ...TAB_NUMBERS.map((n) => ({
    id: `workspace.goToTab${n}` as const,
    label: `Go to tab ${n}`,
    group: "Workspace" as const,
    defaultChord: `Ctrl+Shift+${n}`,
  })),
  {
    id: "workspace.goToLastTab",
    label: "Go to last tab",
    group: "Workspace",
    defaultChord: "Ctrl+Shift+9",
  },
  {
    id: "workspace.focusPaneLeft",
    label: "Focus pane left",
    group: "Workspace",
    defaultChord: "Alt+Shift+Left",
  },
  {
    id: "workspace.focusPaneRight",
    label: "Focus pane right",
    group: "Workspace",
    defaultChord: "Alt+Shift+Right",
  },
  {
    id: "workspace.focusPaneUp",
    label: "Focus pane up",
    group: "Workspace",
    defaultChord: "Alt+Shift+Up",
  },
  {
    id: "workspace.focusPaneDown",
    label: "Focus pane down",
    group: "Workspace",
    defaultChord: "Alt+Shift+Down",
  },
  {
    id: "terminal.jumpPreviousPrompt",
    label: "Jump to previous prompt",
    group: "Terminal",
    defaultChord: "Ctrl+Alt+Up",
  },
  {
    id: "terminal.jumpNextPrompt",
    label: "Jump to next prompt",
    group: "Terminal",
    defaultChord: "Ctrl+Alt+Down",
  },
] as const;

/** actionId -> current chord string. */
export type Keymap = Record<KeymapActionId, string>;

export const DEFAULT_KEYMAP: Keymap = Object.fromEntries(
  KEYMAP_ACTIONS.map((action) => [action.id, action.defaultChord]),
) as Keymap;

export type Chord = {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /** The `event.code` this chord matches, e.g. "KeyB", "Tab", "ArrowUp". */
  code: string;
};

/** Minimal shape of a KeyboardEvent the matcher needs. */
export type ChordEvent = {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  code: string;
};

/** event.code values for the modifier keys themselves (ignored during capture). */
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
  "OSLeft",
  "OSRight",
]);

/** Map an `event.code` to the display/serialization token used in a chord. */
function codeToToken(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3); // KeyB -> B
  if (/^Digit[0-9]$/.test(code)) return code.slice(5); // Digit1 -> 1
  if (code.startsWith("Arrow")) return code.slice(5); // ArrowUp -> Up
  return code; // Tab, PageUp, PageDown, Enter, F1, ...
}

/** Reverse of codeToToken: map a chord token back to the `event.code` it matches. */
function tokenToCode(token: string): string {
  if (/^[A-Za-z]$/.test(token)) return `Key${token.toUpperCase()}`;
  if (/^[0-9]$/.test(token)) return `Digit${token}`;
  if (["Up", "Down", "Left", "Right"].includes(token)) return `Arrow${token}`;
  return token;
}

/** Parse a chord string into its structured form, or null when malformed. */
export function parseChord(chord: string): Chord | null {
  if (typeof chord !== "string") return null;
  const parts = chord
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean);
  if (parts.length === 0) return null;
  const token = parts[parts.length - 1];
  const mods = parts.slice(0, -1).map((mod) => mod.toLowerCase());
  return {
    ctrl: mods.includes("ctrl") || mods.includes("control"),
    alt: mods.includes("alt") || mods.includes("option"),
    shift: mods.includes("shift"),
    meta:
      mods.includes("meta") ||
      mods.includes("cmd") ||
      mods.includes("super") ||
      mods.includes("win"),
    code: tokenToCode(token),
  };
}

/** Whether a chord carries at least one non-Shift modifier. Chords without one
 * are refused so ordinary terminal typing (and bare Shift+letter) is never
 * shadowed by a global shortcut. */
export function hasRequiredModifier(chord: Chord): boolean {
  return chord.ctrl || chord.alt || chord.meta;
}

/** Exact-modifier match of a chord against a keyboard event. */
export function matchesChord(chord: Chord, event: ChordEvent): boolean {
  return (
    event.code === chord.code &&
    event.ctrlKey === chord.ctrl &&
    event.altKey === chord.alt &&
    event.shiftKey === chord.shift &&
    event.metaKey === chord.meta
  );
}

/** Re-serialize a chord in canonical modifier order (Ctrl, Alt, Shift, Meta),
 * so two equivalent strings compare equal. Returns null for malformed input. */
export function canonicalizeChord(chord: string): string | null {
  const parsed = parseChord(chord);
  if (!parsed) return null;
  return formatChordStruct(parsed);
}

function formatChordStruct(chord: Chord): string {
  const parts: string[] = [];
  if (chord.ctrl) parts.push("Ctrl");
  if (chord.alt) parts.push("Alt");
  if (chord.shift) parts.push("Shift");
  if (chord.meta) parts.push("Meta");
  parts.push(codeToToken(chord.code));
  return parts.join("+");
}

/** Build a canonical chord string from a captured keydown, or null when the key
 * pressed is itself only a modifier (capture should keep waiting). */
export function chordFromEvent(event: ChordEvent): string | null {
  if (MODIFIER_CODES.has(event.code)) return null;
  return formatChordStruct({
    ctrl: event.ctrlKey,
    alt: event.altKey,
    shift: event.shiftKey,
    meta: event.metaKey,
    code: event.code,
  });
}

/** Whether a chord is safe/valid to bind: parses and carries a required
 * modifier (rejects bare keys and Shift-only combinations). */
export function isBindableChord(chord: string): boolean {
  const parsed = parseChord(chord);
  return parsed !== null && hasRequiredModifier(parsed);
}

/** The action whose current chord matches this event, honoring registry order
 * for precedence. Unbindable chords (missing required modifier) never fire. */
export function resolveAction(keymap: Keymap, event: ChordEvent): KeymapActionId | null {
  for (const action of KEYMAP_ACTIONS) {
    const chord = parseChord(keymap[action.id]);
    if (!chord || !hasRequiredModifier(chord)) continue;
    if (matchesChord(chord, event)) return action.id;
  }
  return null;
}

/** The other action already bound to `chord`, or null when there is no conflict. */
export function findConflict(
  keymap: Keymap,
  actionId: KeymapActionId,
  chord: string,
): KeymapActionId | null {
  const target = canonicalizeChord(chord);
  if (!target) return null;
  for (const action of KEYMAP_ACTIONS) {
    if (action.id === actionId) continue;
    if (canonicalizeChord(keymap[action.id]) === target) return action.id;
  }
  return null;
}

/** Merge a persisted value with the defaults: unknown action ids are dropped,
 * missing actions fall back to their default, and invalid/unbindable stored
 * chords are ignored (the default is kept). Never throws. */
export function mergeKeymap(stored: unknown): Keymap {
  const result: Keymap = { ...DEFAULT_KEYMAP };
  if (!stored || typeof stored !== "object") return result;
  const record = stored as Record<string, unknown>;
  for (const action of KEYMAP_ACTIONS) {
    const value = record[action.id];
    if (typeof value !== "string") continue;
    const canon = canonicalizeChord(value);
    if (canon && isBindableChord(canon)) result[action.id] = canon;
  }
  return result;
}

/*
 * Actions that only sometimes have something to do (there may be no tab 7, or
 * no pane to the left). Their chords are not withheld from the shell up front;
 * the window handler consumes one only when it actually acted, so in a
 * single-pane tab Alt+Shift+Arrow still reaches tmux or the editor.
 */
export const CONDITIONAL_ACTIONS: ReadonlySet<KeymapActionId> = new Set<KeymapActionId>([
  ...TAB_NUMBERS.map((n) => `workspace.goToTab${n}` as const),
  "workspace.goToLastTab",
  "workspace.focusPaneLeft",
  "workspace.focusPaneRight",
  "workspace.focusPaneUp",
  "workspace.focusPaneDown",
]);

/** Chords xterm must never pass to the shell: every unconditional app action. */
export function keymapChords(keymap: Keymap): string[] {
  return KEYMAP_ACTIONS.filter((action) => !CONDITIONAL_ACTIONS.has(action.id)).map(
    (action) => keymap[action.id],
  );
}

/** Display form of a chord: canonical, with Meta shown as Cmd on macOS. */
export function formatChord(chord: string): string {
  const canon = canonicalizeChord(chord);
  if (!canon) return chord;
  if (isMacUa() && canon.includes("Meta")) {
    return canon.replace(/\bMeta\b/g, "Cmd");
  }
  return canon;
}

function isMacUa(): boolean {
  return typeof navigator !== "undefined" && navigator.userAgent.includes("Mac");
}
