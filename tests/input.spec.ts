import { describe, expect, it } from "vitest";

import {
  INPUT_HEADER_LEN,
  INPUT_MAGIC,
  INPUT_TYPE_KEY,
  INPUT_TYPE_MOUSE_BUTTON,
  INPUT_TYPE_MOUSE_MOVE,
  INPUT_TYPE_UNICODE,
  INPUT_TYPE_WHEEL,
  focusSyncPacket,
  keyPacket,
  mouseButtonPacket,
  mouseMovePacket,
  rdpButton,
  releaseAllPacket,
  unicodePacket,
  wheelPacket,
  wheelUnits,
} from "../src/input";

function view(data: Uint8Array): DataView {
  return new DataView(data.buffer, data.byteOffset, data.byteLength);
}

describe("input protocol", () => {
  it("encodes a key packet", () => {
    const packet = keyPacket(5, 0x1d, true, true);
    const data = view(packet);
    expect(data.getUint8(0)).toBe(INPUT_MAGIC);
    expect(data.getUint8(2)).toBe(INPUT_TYPE_KEY);
    expect(data.getUint32(4, false)).toBe(5);
    expect(data.getUint8(INPUT_HEADER_LEN)).toBe(0x1d);
    expect(data.getUint8(INPUT_HEADER_LEN + 1)).toBe(1);
    expect(data.getUint8(INPUT_HEADER_LEN + 2)).toBe(1);
  });

  it("encodes mouse move and button coordinates", () => {
    const move = view(mouseMovePacket(1, 1920, 1080));
    expect(move.getUint8(2)).toBe(INPUT_TYPE_MOUSE_MOVE);
    expect(move.getUint16(INPUT_HEADER_LEN, false)).toBe(1920);
    expect(move.getUint16(INPUT_HEADER_LEN + 2, false)).toBe(1080);

    const button = view(mouseButtonPacket(1, 2, true, 10, 20));
    expect(button.getUint8(2)).toBe(INPUT_TYPE_MOUSE_BUTTON);
    expect(button.getUint8(INPUT_HEADER_LEN)).toBe(2);
    expect(button.getUint16(INPUT_HEADER_LEN + 2, false)).toBe(10);
  });

  it("maps browser buttons", () => {
    expect(rdpButton(0)).toBe(0);
    expect(rdpButton(1)).toBe(1);
    expect(rdpButton(2)).toBe(2);
    expect(rdpButton(3)).toBe(3);
    expect(rdpButton(4)).toBe(4);
    expect(rdpButton(9)).toBeNull();
  });

  it("maps wheel deltas by delta mode and axis", () => {
    // Pixel deltas keep their magnitude so trackpads scroll smoothly.
    expect(wheelUnits(100)).toEqual({ vertical: true, delta: -100 });
    expect(wheelUnits(-240)).toEqual({ vertical: true, delta: 240 });
    // Lines and pages map to classic notches.
    expect(wheelUnits(3, 0, 1)).toEqual({ vertical: true, delta: -360 });
    expect(wheelUnits(-1, 0, 2)).toEqual({ vertical: true, delta: 360 });
    // The dominant axis wins; positive pixels scroll down (negative RDP units).
    expect(wheelUnits(0, 120)).toEqual({ vertical: false, delta: -120 });
    expect(wheelUnits(-10, 4)).toEqual({ vertical: true, delta: 10 });
    // Pixel deltas are clamped to a sane packet size.
    expect(wheelUnits(1000).delta).toBe(-255);
    expect(wheelUnits(-1000).delta).toBe(255);
    expect(wheelUnits(0)).toEqual({ vertical: true, delta: 0 });
  });

  it("encodes wheel packets", () => {
    const packet = view(wheelPacket(3, true, -120, 100, 200));
    expect(packet.getUint8(2)).toBe(INPUT_TYPE_WHEEL);
    expect(packet.getInt16(INPUT_HEADER_LEN + 1, false)).toBe(-120);
    expect(packet.getUint16(INPUT_HEADER_LEN + 3, false)).toBe(100);
  });

  it("encodes unicode and release packets", () => {
    const unicode = view(unicodePacket(4, 0x4f60, true));
    expect(unicode.getUint8(2)).toBe(INPUT_TYPE_UNICODE);
    expect(unicode.getUint32(INPUT_HEADER_LEN, false)).toBe(0x4f60);
    expect(releaseAllPacket(4).length).toBe(INPUT_HEADER_LEN);
  });

  it("encodes focus sync modifiers", () => {
    const sync = view(focusSyncPacket(8, true, false, true));
    expect(sync.getUint32(4, false)).toBe(8);
    expect(sync.getUint8(INPUT_HEADER_LEN)).toBe(1);
    expect(sync.getUint8(INPUT_HEADER_LEN + 1)).toBe(0);
    expect(sync.getUint8(INPUT_HEADER_LEN + 2)).toBe(1);
  });
});
