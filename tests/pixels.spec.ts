import { describe, expect, it } from "vitest";

import { scaleRgbaNearest } from "../src/pixels";

function pixel(data: Uint8ClampedArray, width: number, x: number, y: number): number[] {
  const offset = (y * width + x) * 4;
  return Array.from(data.slice(offset, offset + 4));
}

describe("scaleRgbaNearest", () => {
  it("keeps a single pixel colour when upscaling", () => {
    const result = scaleRgbaNearest(new Uint8ClampedArray([10, 20, 30, 40]), 1, 1, 2, 2);
    expect(pixel(result, 2, 0, 0)).toEqual([10, 20, 30, 255]);
    expect(pixel(result, 2, 1, 1)).toEqual([10, 20, 30, 255]);
  });

  it("maps corners and centre when scaling 4x3 to 8x6", () => {
    const source = new Uint8ClampedArray(4 * 3 * 4);
    for (let index = 0; index < 12; index += 1) {
      source[index * 4] = index;
      source[index * 4 + 1] = 255 - index;
      source[index * 4 + 2] = index * 2;
      source[index * 4 + 3] = 255;
    }
    const result = scaleRgbaNearest(source, 4, 3, 8, 6);
    expect(pixel(result, 8, 0, 0)).toEqual([0, 255, 0, 255]);
    expect(pixel(result, 8, 7, 5)).toEqual([11, 244, 22, 255]);
    expect(pixel(result, 8, 4, 2)).toEqual([6, 249, 12, 255]);
  });

  it("returns a black opaque bitmap when the source is invalid", () => {
    const result = scaleRgbaNearest(new Uint8ClampedArray(0), 0, 0, 2, 1);
    expect(pixel(result, 2, 0, 0)).toEqual([0, 0, 0, 255]);
    expect(pixel(result, 2, 1, 0)).toEqual([0, 0, 0, 255]);
  });

  it("returns an empty bitmap for empty target dimensions", () => {
    expect(scaleRgbaNearest(new Uint8ClampedArray(4), 1, 1, 0, 1).length).toBe(0);
  });
});
