import { describe, expect, it } from "vitest";

import { CLIPBOARD_HEADER_LEN, imageChunkPacket } from "../src/clipboard";

describe("clipboard image protocol", () => {
  it("encodes the chunk header in network byte order", () => {
    const payload = new Uint8Array([1, 2, 3]);
    const packet = imageChunkPacket(9, 0x11223344, 2, 5, 0x400001, payload);
    expect(packet.length).toBe(CLIPBOARD_HEADER_LEN + payload.length);
    const view = new DataView(packet.buffer);
    expect(view.getUint8(0)).toBe(0xc2);
    expect(view.getUint8(1)).toBe(1);
    expect(view.getUint8(2)).toBe(1);
    expect(view.getUint32(4, false)).toBe(0x11223344);
    expect(view.getUint32(8, false)).toBe(9);
    expect(view.getUint16(12, false)).toBe(2);
    expect(view.getUint16(14, false)).toBe(5);
    expect(view.getUint32(16, false)).toBe(0x400001);
    expect(packet.slice(CLIPBOARD_HEADER_LEN)).toEqual(payload);
  });
});
