import { describe, expect, it } from "vitest";

import {
  FRAME_HEADER_LEN,
  FRAME_MAGIC,
  FRAME_TYPE_BITMAP,
  FRAME_TYPE_RESIZE,
  FrameProtocolError,
  expectedChunkBytes,
  isFullDesktopFrame,
  parseFrameHeader,
  splitFrame,
} from "../src/frameProtocol";

function frame(
  overrides: Partial<{
    type: number;
    sessionId: number;
    seq: number;
    desktopWidth: number;
    desktopHeight: number;
    x: number;
    y: number;
    width: number;
    height: number;
    rowStart: number;
    chunkIndex: number;
    chunkCount: number;
    payload: Uint8Array;
    version: number;
    magic: number;
  }> = {},
): Uint8Array {
  const value = {
    type: FRAME_TYPE_BITMAP,
    sessionId: 7,
    seq: 1,
    desktopWidth: 1920,
    desktopHeight: 1080,
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
    rowStart: 0,
    chunkIndex: 0,
    chunkCount: 1,
    payload: new Uint8Array(1920 * 1080 * 4),
    version: 1,
    magic: FRAME_MAGIC,
    ...overrides,
  };
  const data = new Uint8Array(FRAME_HEADER_LEN + value.payload.length);
  const view = new DataView(data.buffer);
  view.setUint8(0, value.magic);
  view.setUint8(1, value.version);
  view.setUint8(2, value.type);
  view.setUint32(4, value.sessionId, false);
  view.setUint32(8, value.seq, false);
  view.setUint16(12, value.desktopWidth, false);
  view.setUint16(14, value.desktopHeight, false);
  view.setUint16(16, value.x, false);
  view.setUint16(18, value.y, false);
  view.setUint16(20, value.width, false);
  view.setUint16(22, value.height, false);
  view.setUint16(24, value.rowStart, false);
  view.setUint16(26, value.chunkIndex, false);
  view.setUint16(28, value.chunkCount, false);
  data.set(value.payload, FRAME_HEADER_LEN);
  return data;
}

describe("frame protocol", () => {
  it("parses a full bitmap header", () => {
    const header = parseFrameHeader(frame());
    expect(header.sessionId).toBe(7);
    expect(header.desktopWidth).toBe(1920);
    expect(isFullDesktopFrame(header)).toBe(true);
    expect(expectedChunkBytes(header)).toBe(1920 * 1080 * 4);
  });

  it("parses a row-aligned chunk", () => {
    const header = parseFrameHeader(frame({ y: 100, rowStart: 50, height: 200, payload: new Uint8Array(1920 * 150 * 4) }));
    expect(header.rowStart).toBe(50);
    expect(expectedChunkBytes(header)).toBe(1920 * 150 * 4);
    expect(isFullDesktopFrame(header)).toBe(false);
  });

  it("accepts resize frames without payload", () => {
    const fragment = splitFrame(frame({ type: FRAME_TYPE_RESIZE, payload: new Uint8Array(0) }));
    expect(fragment.header.frameType).toBe(FRAME_TYPE_RESIZE);
  });

  it("rejects a bad magic byte", () => {
    expect(() => parseFrameHeader(frame({ magic: 0 }))).toThrow(FrameProtocolError);
  });

  it("rejects an unsupported version", () => {
    expect(() => parseFrameHeader(frame({ version: 9 }))).toThrow(FrameProtocolError);
  });

  it("rejects a payload with the wrong length", () => {
    expect(() => splitFrame(frame({ payload: new Uint8Array(10) }))).toThrow(FrameProtocolError);
  });
});
