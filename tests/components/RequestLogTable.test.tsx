import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  isReasoningTokenWarning,
  RequestLogTable,
} from "@/components/usage/RequestLogTable";
import { formatTimingMs } from "@/components/usage/format";
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

  it.each([
    [8_178, "8.2s"],
    [65_250, "1m 5.3s"],
    [5_208_631, "1h 26m 48.6s"],
  ])("formats %i ms as %s", (milliseconds, expected) => {
    expect(formatTimingMs(milliseconds as number)).toBe(expected);
  });

  it("does not show Codex turn or first-token timing", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [
          makeRequestLog({
            latencyMs: 0,
            durationMs: 5_208_631,
            firstTokenMs: 8_178,
          }),
        ],
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

    expect(screen.getByText("—")).toBeInTheDocument();
    expect(screen.queryByText(/wholeTurn/)).not.toBeInTheDocument();
    expect(screen.queryByText(/firstToken/)).not.toBeInTheDocument();
  });

  it("shows only proxy latency even when first-token timing exists", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [
          makeRequestLog({
            providerId: "provider-1",
            providerName: "Provider",
            dataSource: "proxy",
            latencyMs: 12_957,
            firstTokenMs: 1_234,
          }),
        ],
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

    expect(screen.getByText("13.0s")).toBeInTheDocument();
    expect(screen.queryByText("1.2s")).not.toBeInTheDocument();
  });

  it("shows unavailable instead of zero for Codex request timing", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [makeRequestLog({ latencyMs: 0, durationMs: undefined })],
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

    expect(screen.getByText("—")).toBeInTheDocument();
    expect(screen.queryByText("0.0s")).not.toBeInTheDocument();
  });

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

  // 显示逻辑与 app/provider/模型名无关：任何应用只要 reasoningOutputTokens > 0
  // 就展示同一行；红色规则只看数值本身。
  it.each([
    {
      appType: "claude",
      providerId: "_session",
      providerName: "Claude (Session)",
      model: "claude-sonnet-4-5",
      dataSource: "session_log",
    },
    {
      appType: "gemini",
      providerId: "_gemini_session",
      providerName: "Gemini (Session)",
      model: "gemini-3.6-flash",
      dataSource: "gemini_session",
    },
    {
      appType: "opencode",
      providerId: "_opencode_session",
      providerName: "OpenCode (Session)",
      model: "deepseek-v4-flash",
      dataSource: "opencode_session",
    },
    {
      appType: "grokbuild",
      providerId: "_grok_session",
      providerName: "Grok Build (Session)",
      model: "grok-4.5-build",
      dataSource: "grok_session",
    },
  ])(
    "shows reasoning tokens for non-GPT app $appType with model $model",
    ({ appType, providerId, providerName, model, dataSource }) => {
      useRequestLogsMock.mockReturnValue({
        data: {
          data: [
            makeRequestLog({
              requestId: `reasoning-${appType}`,
              appType,
              providerId,
              providerName,
              model,
              dataSource,
              reasoningOutputTokens: 517,
            }),
          ],
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
          appType={
            appType as "claude" | "codex" | "gemini" | "grokbuild" | "opencode"
          }
          refreshIntervalMs={0}
        />,
      );

      const reasoning = screen.getByText(/Reasoning\s+517/);
      expect(reasoning).toHaveClass("text-muted-foreground");
      expect(reasoning).not.toHaveClass("text-red-600");
    },
  );

  it("highlights suspicious reasoning tokens for non-GPT apps too", () => {
    useRequestLogsMock.mockReturnValue({
      data: {
        data: [
          makeRequestLog({
            requestId: "reasoning-claude-suspicious",
            appType: "claude",
            providerId: "_session",
            providerName: "Claude (Session)",
            model: "claude-opus-4-6",
            dataSource: "proxy",
            reasoningOutputTokens: 1552,
          }),
        ],
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
        appType="claude"
        refreshIntervalMs={0}
      />,
    );

    const reasoning = screen.getByText(/Reasoning\s+1,552/);
    expect(reasoning).toHaveClass("text-red-600");
  });
});
