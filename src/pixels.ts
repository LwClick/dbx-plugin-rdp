/**
 * Nearest-neighbour RGBA scaling used when the remote framebuffer is
 * recreated at a new size. Keeping the previous picture avoids a black gap
 * while the server repaints after a display resize.
 */
export function scaleRgbaNearest(
  source: Uint8ClampedArray,
  sourceWidth: number,
  sourceHeight: number,
  targetWidth: number,
  targetHeight: number,
): Uint8ClampedArray {
  if (targetWidth <= 0 || targetHeight <= 0) return new Uint8ClampedArray(0);
  const target = new Uint8ClampedArray(targetWidth * targetHeight * 4);
  const sourcePixels = sourceWidth * sourceHeight;
  if (
    sourceWidth <= 0 ||
    sourceHeight <= 0 ||
    source.length < sourcePixels * 4
  ) {
    for (let index = 3; index < target.length; index += 4) {
      target[index] = 255;
    }
    return target;
  }

  const xMap = new Uint32Array(targetWidth);
  for (let x = 0; x < targetWidth; x += 1) {
    xMap[x] = Math.min(sourceWidth - 1, Math.floor((x * sourceWidth) / targetWidth));
  }

  for (let y = 0; y < targetHeight; y += 1) {
    const sourceY = Math.min(sourceHeight - 1, Math.floor((y * sourceHeight) / targetHeight));
    const sourceRow = sourceY * sourceWidth;
    const targetRow = y * targetWidth;
    for (let x = 0; x < targetWidth; x += 1) {
      const sourceOffset = (sourceRow + xMap[x]) * 4;
      const targetOffset = (targetRow + x) * 4;
      target[targetOffset] = source[sourceOffset];
      target[targetOffset + 1] = source[sourceOffset + 1];
      target[targetOffset + 2] = source[sourceOffset + 2];
      target[targetOffset + 3] = 255;
    }
  }
  return target;
}
