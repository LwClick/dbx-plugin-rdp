import { CTRL, type RdpScancode } from "./scancodes";

export interface ShortcutKey {
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export interface MacShortcut extends RdpScancode {
  ctrl: boolean;
  shift: boolean;
}

/** Cmd+<key> -> remote Ctrl+<key>. Physical scancodes, US layout positions. */
const COMMAND_KEYS: Record<string, number> = {
  KeyC: 0x2e,
  KeyX: 0x2d,
  KeyA: 0x1e,
  KeyZ: 0x2c,
  KeyS: 0x1f,
  KeyF: 0x21,
  KeyP: 0x19,
  KeyY: 0x15,
};

/** macOS line navigation -> Windows Home/End. */
const COMMAND_NAV: Record<string, RdpScancode> = {
  ArrowLeft: { code: 0x47, extended: true },
  ArrowRight: { code: 0x4f, extended: true },
};

/** Host/system shortcuts we must leave alone. */
const EXCLUDED = new Set([
  "KeyV", // handled by the clipboard paste flow
  "KeyN",
  "KeyO",
  "KeyW",
  "KeyQ",
  "KeyH",
  "KeyM",
  "Tab",
  "Space",
  "Backquote",
]);

/**
 * Maps a macOS Command shortcut to the Windows equivalent.
 *
 * Returns `null` when remapping is disabled, the combination is reserved by
 * the host, or no equivalent is defined.
 */
export function remapMacCommand(
  event: ShortcutKey,
  enabled: boolean,
): MacShortcut | null {
  if (!enabled || !event.metaKey || event.ctrlKey || event.altKey) return null;
  if (EXCLUDED.has(event.code)) return null;

  const nav = COMMAND_NAV[event.code];
  if (nav) {
    return { ...nav, ctrl: false, shift: false };
  }

  const code = COMMAND_KEYS[event.code];
  if (code === undefined) return null;
  // Ctrl+Shift+Z is the Windows redo shortcut; other shifted variants keep
  // their Shift modifier.
  return { code, extended: false, ctrl: true, shift: event.shiftKey };
}

export function shortcutTapKeys(shortcut: MacShortcut): RdpScancode[] {
  const keys: RdpScancode[] = [];
  if (shortcut.ctrl) keys.push(CTRL);
  if (shortcut.shift) keys.push({ code: 0x2a, extended: false });
  keys.push({ code: shortcut.code, extended: shortcut.extended });
  return keys;
}
