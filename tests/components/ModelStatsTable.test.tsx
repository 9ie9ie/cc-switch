import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ModelStatsTable } from "@/components/usage/ModelStatsTable";
import type { ModelStats } from "@/types/usage";

const useModelStatsMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { resolvedLanguage: "en", language: "en" },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useModelStats: (...args: unknown[]) => useModelStatsMock(...args),
}));

const stat = (overrides: Partial<ModelStats>): ModelStats => ({
  model: "deepseek-v4-pro",
  requestCount: 1,
  totalTokens: 2_500,
  totalCost: "0.01",
  avgCostPerRequest: "0.01",
  successRate: 100,
  ...overrides,
});

describe("ModelStatsTable", () => {
  it("retains exact and estimated speeds while hiding both first-token medians", () => {
    useModelStatsMock.mockReturnValue({
      isLoading: false,
      data: [
        stat({
          requestCount: 2,
          speedOutputTokens: 1_200,
          speedGenerationMs: 10_000,
          firstTokenMedianMs: 2_600,
          estFirstTokenMedianMs: 3_300,
        }),
        stat({
          model: "kimi-k2.6",
          estSpeedOutputTokens: 1_800,
          estSpeedDurationMs: 20_000,
          estFirstTokenMedianMs: 3_300,
        }),
      ],
    });
    render(<ModelStatsTable range={{ preset: "7d" }} refreshIntervalMs={0} />);

    expect(screen.getAllByRole("columnheader")).toHaveLength(6);
    expect(
      screen.queryByRole("columnheader", { name: /firstToken/ }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/2\.6s|3\.3s/)).not.toBeInTheDocument();
    const rows = screen.getAllByRole("row").slice(1);
    expect(rows[0].lastElementChild).toHaveTextContent("120tok/s");
    expect(rows[1].lastElementChild).toHaveTextContent("≈90tok/s");
    for (const row of rows) expect(row.children).toHaveLength(6);
  });

  it("keeps the empty state aligned with the remaining columns", () => {
    useModelStatsMock.mockReturnValue({ isLoading: false, data: [] });
    render(<ModelStatsTable range={{ preset: "7d" }} refreshIntervalMs={0} />);
    expect(screen.getByRole("cell", { name: "usage.noData" })).toHaveAttribute(
      "colspan",
      "6",
    );
  });
});
