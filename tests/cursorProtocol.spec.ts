import { describe, expect, it } from "vitest";

import {
  CURSOR_HEADER_LEN,
  CURSOR_KIND_BITMAP,
  CURSOR_KIND_DEFAULT,
  CURSOR_KIND_HIDDEN,
  CURSOR_MAGIC,
  CURSOR_PROTOCOL_VERSION,
  CursorProtocolError,
  parseCursorMessage,
} from "../src/cursorProtocol";

function cursor(
  overrides: Partial<{
    kind: number;
    sessionId: number;
    hotspotX: number;
    hotspotY: number;
    width: number;
    height: number;
    bitmap: Uint8Array;
    magic: number;
    version: number;
  }> = {},
): Uint8Array {
  const value = {
    kind: CURSOR_KIND_DEFAULT,
    sessionId: 7,
    hotspotX: 0,
    hotspotY: 0,
    width: 0,
    height: 0,
    bitmap: new Uint8Array(0),
    magic: CURSOR_MAGIC,
    version: CURSOR_PROTOCOL_VERSION,
    ...overrides,
  };
  const data = new Uint8Array(CURSOR_HEADER_LEN + value.bitmap.length);
  const view = new DataView(data.buffer);
  view.setUint8(0, value.magic);
  view.setUint8(1, value.version);
  view.setUint8(2, value.kind);
  view.setUint32(4, value.sessionId, false);
  view.setUint16(8, value.hotspotX, false);
  view.setUint16(10, value.hotspotY, false);
  view.setUint16(12, value.width, false);
  view.setUint16(14, value.height, false);
  data.set(value.bitmap, CURSOR_HEADER_LEN);
  return data;
}

describe("cursor protocol", () => {
  it("parses the default cursor", () => {
    const message = parseCursorMessage(cursor());
    expect(message.kind).toBe("default");
    expect(message.sessionId).toBe(7);
  });

  it("parses a hidden cursor", () => {
    expect(parseCursorMessage(cursor({ kind: CURSOR_KIND_HIDDEN })).kind).toBe("hidden");
  });

  it("parses a bitmap cursor with its hotspot", () => {
    const bitmap = new Uint8Array(2 * 2 * 4).fill(0x7f);
    const message = parseCursorMessage(
      cursor({
        kind: CURSOR_KIND_BITMAP,
        hotspotX: 3,
        hotspotY: 4,
        width: 2,
        height: 2,
        bitmap,
      }),
    );
    expect(message.kind).toBe("bitmap");
    expect(message.hotspotX).toBe(3);
    expect(message.hotspotY).toBe(4);
    expect(message.width).toBe(2);
    expect(message.height).toBe(2);
    expect(message.bitmap).toEqual(bitmap);
  });

  it("rejects a bad magic byte", () => {
    expect(() => parseCursorMessage(cursor({ magic: 0 }))).toThrow(CursorProtocolError);
  });

  it("rejects an unsupported version", () => {
    expect(() => parseCursorMessage(cursor({ version: 9 }))).toThrow(CursorProtocolError);
  });

  it("rejects an unknown kind", () => {
    expect(() => parseCursorMessage(cursor({ kind: 9 }))).toThrow(CursorProtocolError);
  });

  it("rejects a bitmap with the wrong payload length", () => {
    expect(() =>
      parseCursorMessage(
        cursor({
          kind: CURSOR_KIND_BITMAP,
          width: 2,
          height: 2,
          bitmap: new Uint8Array(4),
        }),
      ),
    ).toThrow(CursorProtocolError);
  });
});
