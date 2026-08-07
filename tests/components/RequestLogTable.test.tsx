import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  isReasoningTokenWarning,
  RequestLogTable,
} from "@/components/usage/RequestLogTable";
import type { RequestLog, UsageRangeSelection } from "@/types/usage";

const useRequestLogsMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (
      key: string,
      options?: {
        defaultValue?: string;
      },
    ) => options?.defaultValue ?? key,
    i18n: {
      resolvedLanguage: "en",
      language: "en",
    },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useRequestLogs: (args: unknown) => useRequestLogsMock(args),
}));

vi.mock("@/components/ui/button", () => ({
  Button: ({ children, ...props }: any) => (
    <button {...props}>{children}</button>
  ),
}));

vi.mock("@/components/ui/input", () => ({
  Input: (props: any) => <input {...props} />,
}));

vi.mock("@/components/ui/select", () => ({
  Select: ({ children }: any) => <div>{children}</div>,
  SelectTrigger: ({ children, ...props }: any) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
  SelectValue: ({ placeholder }: any) => <span>{placeholder ?? null}</span>,
  SelectContent: () => null,
  SelectItem: () => null,
}));

vi.mock("@/components/ui/table", () => ({
  Table: ({ children }: any) => <table>{children}</table>,
  TableBody: ({ children }: any) => <tbody>{children}</tbody>,
  TableCell: ({ children, ...props }: any) => <td {...props}>{children}</td>,
  TableHead: ({ children, ...props }: any) => <th {...props}>{children}</th>,
  TableHeader: ({ children }: any) => <thead>{children}</thead>,
  TableRow: ({ children }: any) => <tr>{children}</tr>,
}));

const makeRequestLog = (overrides: Partial<RequestLog> = {}): RequestLog => ({
  requestId: "codex-session-1",
  providerId: "_codex_session",
  providerName: "Codex (Session)",
  appType: "codex",
  model: "gpt-5.6",
  costMultiplier: "1.0",
  inputTokens: 1000,
  outputTokens: 800,
  cacheReadTokens: 100,
  cacheCreationTokens: 0,
  inputCostUsd: "0",
  outputCostUsd: "0",
  cacheReadCostUsd: "0",
  cacheCreationCostUsd: "0",
  totalCostUsd: "0",
  isStreaming: true,
  latencyMs: 1200,
  statusCode: 200,
  createdAt: 1_710_000_000,
  dataSource: "codex_session",
  ...overrides,
});

describe("RequestLogTable", () => {
  beforeEach(() => {
    useRequestLogsMock.mockReset();
    useRequestLogsMock.mockImplementation(
      ({ page = 0, pageSize = 20 }: { page?: number; pageSize?: number }) => ({
        data: {
          data: [],
          total: 120,
          page,
          pageSize,
        },
        isLoading: false,
      }),
    );
  });

  it.each([516, 1034, 1552, 2070, 2588, 51_798])(
    "marks %i reasoning tokens as suspicious",
    (tokens) => {
      expect(isReasoningTokenWarning(tokens)).toBe(true);
    },
  );

  it.each([0, -2, 515, 517, 516.5])(
    "does not mark %s reasoning tokens as suspicious",
    (tokens) => {
      expect(isReasoningTokenWarning(tokens)).toBe(false);
    },
  );

  it("resets pagination when the dashboard range changes", async () => {
    const initialRange: UsageRangeSelection = { preset: "today" };
    const nextRange: UsageRangeSelection = {
      preset: "custom",
      customStartDate: 1_710_000_000,
      customEndDate: 1_710_086_400,
    };

    const { rerender } = render(
      <RequestLogTable
        range={initialRange}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "2" }));

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 1,
          range: initialRange,
        }),
      );
    });

    rerender(
      <RequestLogTable
        range={nextRange}
        rangeLabel="Custom"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 0,
          range: nextRange,
        }),
      );
    });
  });

  it("resets pagination when the dashboard app filter changes", async () => {
    const range: UsageRangeSelection = { preset: "today" };
    const { rerender } = render(
      <RequestLogTable
        range={range}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "2" }));

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 1,
          range,
        }),
      );
    });

    rerender(
      <RequestLogTable
        range={range}
        rangeLabel="Today"
        appType="claude"
        refreshIntervalMs={0}
      />,
    );

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 0,
          range,
        }),
      );
    });
  });

  it("shows and highlights matching reasoning tokens", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [makeRequestLog({ reasoningOutputTokens: 2588 })],
        total: 1,
        page: 0,
        pageSize: 20,
      },
      isLoading: false,
    });

    render(
      <RequestLogTable
        range={{ preset: "today" }}
        rangeLabel="Today"
        appType="codex"
        refreshIntervalMs={0}
      />,
    );

    const reasoning = screen.getByText(/Reasoning\s+2,588/);
    expect(reasoning).toHaveClass("text-red-600");
  });

  it("shows non-matching reasoning tokens without highlighting them", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [makeRequestLog({ reasoningOutputTokens: 517 })],
        total: 1,
        page: 0,
        pageSize: 20,
      },
      isLoading: false,
    });

    render(
      <RequestLogTable
        range={{ preset: "today" }}
        rangeLabel="Today"
        appType="codex"
        refreshIntervalMs={0}
      />,
    );

    const reasoning = screen.getByText(/Reasoning\s+517/);
    expect(reasoning).toHaveClass("text-muted-foreground");
    expect(reasoning).not.toHaveClass("text-red-600");
  });

  it.each([undefined, 0])(
    "does not show missing or zero reasoning tokens (%s)",
    (reasoningOutputTokens) => {
      useRequestLogsMock.mockReturnValue({
        data: {
          data: [makeRequestLog({ reasoningOutputTokens })],
          total: 1,
          page: 0,
          pageSize: 20,
        },
        isLoading: false,
      });

      render(
        <RequestLogTable
          range={{ preset: "today" }}
          rangeLabel="Today"
          appType="codex"
          refreshIntervalMs={0}
        />,
      );

      expect(screen.queryByText(/^Reasoning\b/)).not.toBeInTheDocument();
    },
  );
});
