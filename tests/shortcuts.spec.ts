import { describe, expect, it } from "vitest";

import { CTRL } from "../src/scancodes";
import { remapMacCommand, shortcutTapKeys, type ShortcutKey } from "../src/shortcuts";

function command(code: string, extra: Partial<ShortcutKey> = {}): ShortcutKey {
  return { code, metaKey: true, ctrlKey: false, altKey: false, shiftKey: false, ...extra };
}

describe("macOS Command remapping", () => {
  it("maps common editing shortcuts to Ctrl", () => {
    expect(remapMacCommand(command("KeyC"), true)).toEqual({
      code: 0x2e,
      extended: false,
      ctrl: true,
      shift: false,
    });
    expect(remapMacCommand(command("KeyA"), true)?.code).toBe(0x1e);
    expect(remapMacCommand(command("KeyZ"), true)?.code).toBe(0x2c);
    expect(remapMacCommand(command("KeyS"), true)?.code).toBe(0x1f);
    expect(remapMacCommand(command("KeyF"), true)?.code).toBe(0x21);
  });

  it("keeps Shift for shifted shortcuts", () => {
    const redo = remapMacCommand(command("KeyZ", { shiftKey: true }), true);
    expect(redo).toMatchObject({ ctrl: true, shift: true });
    expect(shortcutTapKeys(redo!)).toEqual([
      CTRL,
      { code: 0x2a, extended: false },
      { code: 0x2c, extended: false },
    ]);
  });

  it("maps line navigation to Home and End", () => {
    expect(remapMacCommand(command("ArrowLeft"), true)).toEqual({
      code: 0x47,
      extended: true,
      ctrl: false,
      shift: false,
    });
    expect(remapMacCommand(command("ArrowRight"), true)?.code).toBe(0x4f);
    expect(shortcutTapKeys(remapMacCommand(command("ArrowLeft"), true)!)).toEqual([
      { code: 0x47, extended: true },
    ]);
  });

  it("leaves host shortcuts and unsupported keys alone", () => {
    for (const code of ["KeyV", "KeyW", "KeyQ", "KeyN", "KeyO", "Tab", "Space", "Backquote"]) {
      expect(remapMacCommand(command(code), true)).toBeNull();
    }
  });

  it("requires Command only and an enabled mapping", () => {
    expect(remapMacCommand(command("KeyC"), false)).toBeNull();
    expect(remapMacCommand(command("KeyC", { metaKey: false }), true)).toBeNull();
    expect(remapMacCommand(command("KeyC", { ctrlKey: true }), true)).toBeNull();
    expect(remapMacCommand(command("KeyC", { altKey: true }), true)).toBeNull();
  });
});
