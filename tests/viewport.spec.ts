import { describe, expect, it } from "vitest";

import { targetDesktopSize } from "../src/viewport";

describe("targetDesktopSize", () => {
  it("keeps the workbench CSS pixels", () => {
    expect(targetDesktopSize(1998, 1071)).toEqual({ width: 1998, height: 1070 });
  });

  it("caps large sizes while keeping the aspect ratio and even dimensions", () => {
    const size = targetDesktopSize(3000, 1800);
    expect(size).toEqual({ width: 2560, height: 1536 });
    expect(size.width % 2).toBe(0);
    expect(size.height % 2).toBe(0);
  });

  it("falls back to the minimum desktop size for an empty stage", () => {
    expect(targetDesktopSize(0, 0)).toEqual({ width: 1024, height: 600 });
  });

  it("does not exceed the maximum for very large stages", () => {
    const size = targetDesktopSize(5000, 3000);
    expect(size.width).toBeLessThanOrEqual(2560);
    expect(size.height).toBeLessThanOrEqual(1600);
  });
});
