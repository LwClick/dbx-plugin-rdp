export const FRAME_CHANNEL = "rdp-frame-v1";
export const INPUT_CHANNEL = "rdp-input-v1";

export const FRAME_HEADER_LEN = 32;
export const FRAME_MAGIC = 0xd1;
export const PROTOCOL_VERSION = 1;
export const FRAME_TYPE_BITMAP = 1;
export const FRAME_TYPE_RESIZE = 2;

export interface FrameHeader {
  frameType: number;
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
}

export class FrameProtocolError extends Error {}

export function parseFrameHeader(data: Uint8Array): FrameHeader {
  if (data.length < FRAME_HEADER_LEN) {
    throw new FrameProtocolError("RDP frame is truncated");
  }
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  if (view.getUint8(0) !== FRAME_MAGIC) {
    throw new FrameProtocolError("RDP frame has a bad magic byte");
  }
  if (view.getUint8(1) !== PROTOCOL_VERSION) {
    throw new FrameProtocolError("RDP frame uses an unsupported protocol version");
  }
  return {
    frameType: view.getUint8(2),
    sessionId: view.getUint32(4, false),
    seq: view.getUint32(8, false),
    desktopWidth: view.getUint16(12, false),
    desktopHeight: view.getUint16(14, false),
    x: view.getUint16(16, false),
    y: view.getUint16(18, false),
    width: view.getUint16(20, false),
    height: view.getUint16(22, false),
    rowStart: view.getUint16(24, false),
    chunkIndex: view.getUint16(26, false),
    chunkCount: view.getUint16(28, false),
  };
}

export function chunkRows(header: FrameHeader): number {
  return Math.max(0, header.height - header.rowStart);
}

export function expectedChunkBytes(header: FrameHeader): number {
  return header.width * chunkRows(header) * 4;
}

export function isFullDesktopFrame(header: FrameHeader): boolean {
  return header.x === 0 && header.y === 0 && header.width === header.desktopWidth && header.height === header.desktopHeight;
}

export interface FrameFragment {
  header: FrameHeader;
  payload: Uint8Array;
}

export function splitFrame(data: Uint8Array): FrameFragment {
  const header = parseFrameHeader(data);
  const payload = data.subarray(FRAME_HEADER_LEN);
  if (header.frameType === FRAME_TYPE_BITMAP) {
    const expected = expectedChunkBytes(header);
    if (payload.length !== expected) {
      throw new FrameProtocolError(`RDP frame payload is ${payload.length} bytes, expected ${expected}`);
    }
  }
  return { header, payload };
}
