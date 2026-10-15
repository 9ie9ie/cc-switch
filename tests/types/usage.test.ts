import { describe, expect, it } from "vitest";
import {
  getCacheWriteAvailability,
  hasUsageTokens,
  isUnpricedUsage,
} from "@/types/usage";

describe("getCacheWriteAvailability", () => {
  it("distinguishes cache-write support across fixed protocols", () => {
    expect(getCacheWriteAvailability(["claude"])).toBe("ok");
    expect(getCacheWriteAvailability(["pi"])).toBe("partial");
    expect(getCacheWriteAvailability(["codex", "gemini"])).toBe("na");
    expect(getCacheWriteAvailability(["claude", "codex"])).toBe("partial");
    expect(getCacheWriteAvailability([])).toBe("ok");
  });
});

describe("reasoning-only usage", () => {
  it.each([undefined, 0, 516])(
    "preserves reasoning count %s when detecting unpriced usage",
    (reasoningOutputTokens) => {
      const log = {
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        reasoningOutputTokens,
        totalCostUsd: "0",
        statusCode: 200,
      };
      expect(hasUsageTokens(log)).toBe(reasoningOutputTokens === 516);
      expect(isUnpricedUsage(log)).toBe(reasoningOutputTokens === 516);
    },
  );
});
