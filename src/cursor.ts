import type { CursorMessage } from "./cursorProtocol";

/** WebKit/Chromium ignore CSS cursor images above roughly 128 px. */
const MAX_CURSOR_SIZE = 128;

export function cursorCss(message: CursorMessage): string {
  if (message.kind === "hidden") return "none";
  if (message.kind !== "bitmap") return "default";
  const { bitmap, width, height } = message;
  if (!bitmap || width <= 0 || height <= 0 || bitmap.length !== width * height * 4) {
    return "default";
  }
  try {
    const source = document.createElement("canvas");
    source.width = width;
    source.height = height;
    const sourceContext = source.getContext("2d");
    if (!sourceContext) return "default";
    const image = sourceContext.createImageData(width, height);
    image.data.set(bitmap);
    sourceContext.putImageData(image, 0, 0);

    let canvas = source;
    let hotspotX = message.hotspotX;
    let hotspotY = message.hotspotY;
    const largest = Math.max(width, height);
    if (largest > MAX_CURSOR_SIZE) {
      const scale = MAX_CURSOR_SIZE / largest;
      const targetWidth = Math.max(1, Math.round(width * scale));
      const targetHeight = Math.max(1, Math.round(height * scale));
      const target = document.createElement("canvas");
      target.width = targetWidth;
      target.height = targetHeight;
      const targetContext = target.getContext("2d");
      if (!targetContext) return "default";
      targetContext.imageSmoothingEnabled = false;
      targetContext.drawImage(source, 0, 0, targetWidth, targetHeight);
      canvas = target;
      hotspotX = Math.round(hotspotX * scale);
      hotspotY = Math.round(hotspotY * scale);
    }

    const url = canvas.toDataURL("image/png");
    if (!url) return "default";
    const x = Math.min(Math.max(0, hotspotX), canvas.width - 1);
    const y = Math.min(Math.max(0, hotspotY), canvas.height - 1);
    return `url("${url}") ${x} ${y}, default`;
  } catch {
    return "default";
  }
}
