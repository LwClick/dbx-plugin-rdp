import { describe, expect, it } from "vitest";

import { createToken, formatBytes } from "../src/files";

describe("formatBytes", () => {
  it("formats common sizes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(12 * 1024 * 1024)).toBe("12 MB");
    expect(formatBytes(4.5 * 1024 * 1024 * 1024)).toBe("4.5 GB");
  });

  it("handles invalid input", () => {
    expect(formatBytes(Number.NaN)).toBe("0 B");
    expect(formatBytes(-1)).toBe("0 B");
  });
});

describe("createToken", () => {
  it("creates sidecar-safe unique tokens", () => {
    const tokens = new Set<string>();
    for (let index = 0; index < 100; index += 1) {
      const token = createToken();
      expect(token).toMatch(/^[a-z0-9]{8,64}$/);
      tokens.add(token);
    }
    expect(tokens.size).toBe(100);
  });
});
