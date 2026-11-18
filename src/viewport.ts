export interface DesktopSize {
  width: number;
  height: number;
}

/**
 * Maps the workbench CSS box to the RDP desktop size. The remote surface is
 * requested in CSS pixels; the canvas scales it to the stage.
 */
export function targetDesktopSize(cssWidth: number, cssHeight: number): DesktopSize {
  const maxWidth = 2560;
  const maxHeight = 1600;
  let width = Math.round(cssWidth);
  let height = Math.round(cssHeight);
  const fit = Math.min(1, maxWidth / Math.max(1, width), maxHeight / Math.max(1, height));
  width = Math.max(1024, Math.floor((width * fit) / 2) * 2);
  height = Math.max(600, Math.floor((height * fit) / 2) * 2);
  return { width, height };
}
