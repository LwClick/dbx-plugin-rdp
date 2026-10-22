import { describe, expect, it } from "vitest";

import { scancodeForCode } from "../src/scancodes";

describe("scancodes", () => {
  it("maps letters, digits and punctuation", () => {
    expect(scancodeForCode("KeyV")).toEqual({ code: 0x2f, extended: false });
    expect(scancodeForCode("Digit1")).toEqual({ code: 0x02, extended: false });
    expect(scancodeForCode("Backquote")).toEqual({ code: 0x29, extended: false });
  });

  it("maps extended navigation keys", () => {
    expect(scancodeForCode("ArrowUp")).toEqual({ code: 0x48, extended: true });
    expect(scancodeForCode("Delete")).toEqual({ code: 0x53, extended: true });
    expect(scancodeForCode("MetaLeft")).toEqual({ code: 0x5b, extended: true });
    expect(scancodeForCode("NumpadEnter")).toEqual({ code: 0x1c, extended: true });
  });

  it("maps function and numpad keys", () => {
    expect(scancodeForCode("F1")).toEqual({ code: 0x3b, extended: false });
    expect(scancodeForCode("F12")).toEqual({ code: 0x58, extended: false });
    expect(scancodeForCode("Numpad5")).toEqual({ code: 0x4c, extended: false });
  });

  it("returns null for unknown keys", () => {
    expect(scancodeForCode("BrowserBack")).toBeNull();
  });
});
