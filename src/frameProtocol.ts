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

/**
 * Derives the row count of one bitmap chunk from its payload. The wire header
 * only carries `rowStart`, not the chunk's row count, and every chunk except
 * the last is shorter than the remaining rectangle height.
 */
export function chunkRowCount(header: FrameHeader, payloadLength: number): number {
  const rowBytes = header.width * 4;
  if (rowBytes <= 0 || payloadLength <= 0 || payloadLength % rowBytes !== 0) {
    throw new FrameProtocolError(`RDP chunk payload is not row-aligned (${payloadLength} bytes)`);
  }
  const rows = payloadLength / rowBytes;
  if (header.rowStart + rows > header.height) {
    throw new FrameProtocolError(
      `RDP chunk rows ${rows} exceed rect height ${header.height} at row ${header.rowStart}`,
    );
  }
  return rows;
}

export function isFullDesktopFrame(header: FrameHeader): boolean {
  return header.x === 0 && header.y === 0 && header.width === header.desktopWidth && header.height === header.desktopHeight;
}

export function frameMatchesSurface(header: FrameHeader, width: number, height: number): boolean {
  return header.desktopWidth === width && header.desktopHeight === height;
}

export interface FrameFragment {
  header: FrameHeader;
  payload: Uint8Array;
}

export function splitFrame(data: Uint8Array): FrameFragment {
  const header = parseFrameHeader(data);
  const payload = data.subarray(FRAME_HEADER_LEN);
  if (header.frameType === FRAME_TYPE_BITMAP) {
    const rows = chunkRowCount(header, payload.length);
    if (rows === 0 || header.chunkCount === 0) {
      throw new FrameProtocolError("RDP bitmap frame has no rows");
    }
  }
  return { header, payload };
}
