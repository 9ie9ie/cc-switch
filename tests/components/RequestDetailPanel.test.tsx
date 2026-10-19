import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RequestDetailPanel } from "@/components/usage/RequestDetailPanel";
import type { RequestLog } from "@/types/usage";

const useRequestDetailMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options && "value" in options ? `${key}:${options.value}` : key,
    i18n: { resolvedLanguage: "en", language: "en" },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useRequestDetail: (id: string) => useRequestDetailMock(id),
}));

const log = (overrides: Partial<RequestLog>): RequestLog => ({
  requestId: "req_1",
  providerId: "p1",
  providerName: "GitHub Copilot",
  appType: "claude",
  model: "claude-opus-5-5",
  costMultiplier: "1",
  inputTokens: 3_066,
  outputTokens: 1_512,
  cacheReadTokens: 71_300,
  cacheCreationTokens: 2_048,
  inputCostUsd: "0.0123",
  outputCostUsd: "0.0302",
  cacheReadCostUsd: "0.0143",
  cacheCreationCostUsd: "0.0102",
  totalCostUsd: "0.0670",
  isStreaming: true,
  latencyMs: 26_400,
  firstTokenMs: 3_200,
  statusCode: 200,
  createdAt: 1_759_212_000,
  ...overrides,
});

const renderWith = (request: RequestLog) => {
  useRequestDetailMock.mockReturnValue({ data: request, isLoading: false });
  render(
    <RequestDetailPanel requestId={request.requestId} onClose={vi.fn()} />,
  );
};

describe("RequestDetailPanel", () => {
  it("shows speed and ordinary latency, but no first-token or whole-turn timing", () => {
    renderWith(log({}));

    expect(screen.getByText("usage.requestDetail")).toBeInTheDocument();
    // 1512 / (26.4 - 3.2) ≈ 65
    expect(screen.getByText("usage.speedValue:65")).toBeInTheDocument();
    expect(screen.queryByText("3.2s")).not.toBeInTheDocument();
    expect(screen.getByText("26.4s")).toBeInTheDocument();
    expect(screen.getByText("usage.latency")).toBeInTheDocument();
    expect(screen.queryByText(/firstToken|duration/i)).not.toBeInTheDocument();
    expect(screen.getByText("$0.0670")).toBeInTheDocument();
  });

  it("explains why short outputs have no speed", () => {
    renderWith(
      log({ outputTokens: 64, latencyMs: 2_400, firstTokenMs: 1_900 }),
    );

    expect(screen.getByText("usage.detail.speedTooFew")).toBeInTheDocument();
  });

  it("retains estimated speed without showing session timing", () => {
    renderWith(
      log({
        dataSource: "session_log",
        firstTokenMs: undefined,
        outputTokens: 1_800,
        latencyMs: 20_000,
      }),
    );

    // 1800 / 20 s，含首字等待
    expect(
      screen.getByText("usage.speedEstimatedValue:90"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("usage.detail.durationEstimated"),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("20.0s")).not.toBeInTheDocument();
    expect(screen.queryByText("usage.detail.firstToken")).toBeNull();
  });

  it.each([undefined, 4_000])(
    "shows sidebar estimated speed with reasoning warnings without displaying firstTokenMs %s or turn timing",
    (firstTokenMs) => {
      renderWith(
        log({
          appType: "codex",
          providerName: "Codex (Sidebar)",
          dataSource: "codex_sidebar",
          outputTokens: 1_000,
          reasoningOutputTokens: 516,
          latencyMs: 10_000,
          firstTokenMs,
          durationMs: 5_208_631,
        }),
      );

      expect(
        screen.getByText("usage.speedEstimatedValue:100"),
      ).toBeInTheDocument();
      expect(screen.queryByText(/^usage\.speedValue:/)).not.toBeInTheDocument();
      expect(screen.getByText("516")).toHaveClass("text-danger-text");
      const latency = screen.getByText("usage.latency").parentElement;
      expect(latency?.querySelector("dd")).toHaveTextContent(/^—$/);
      expect(
        screen.queryByText(
          /10\.0s|4\.0s|1h 26m|firstToken|duration|whole.?turn/i,
        ),
      ).not.toBeInTheDocument();
    },
  );

  it("does not use a long sidebar duration when the request has no latency estimate", () => {
    renderWith(
      log({
        appType: "codex",
        dataSource: "codex_sidebar",
        outputTokens: 1_000,
        latencyMs: 0,
        firstTokenMs: undefined,
        durationMs: 5_208_631,
      }),
    );

    expect(
      screen.getByText("usage.detail.noTimingSession"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/^usage\.speed(?:Estimated)?Value:/),
    ).not.toBeInTheDocument();
    const latency = screen.getByText("usage.latency").parentElement;
    expect(latency?.querySelector("dd")).toHaveTextContent(/^—$/);
    expect(
      screen.queryByText(/0\.0s|1h 26m|firstToken|duration|whole.?turn/i),
    ).not.toBeInTheDocument();
  });

  it("hides estimated first-token timing even when it is present", () => {
    renderWith(
      log({
        dataSource: "codex_session",
        firstTokenMs: 3_300,
        outputTokens: 1_800,
        latencyMs: 20_000,
      }),
    );

    expect(
      screen.queryByText("usage.detail.firstTokenEstimated"),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("3.3s")).not.toBeInTheDocument();
    // 速度仍按整段耗时估
    expect(
      screen.getByText("usage.speedEstimatedValue:90"),
    ).toBeInTheDocument();
    expect(screen.queryByText("usage.detail.firstToken")).toBeNull();
  });

  it("explains why short session-log outputs have no estimate", () => {
    renderWith(
      log({
        dataSource: "session_log",
        firstTokenMs: undefined,
        outputTokens: 120,
        latencyMs: 6_000,
      }),
    );

    expect(
      screen.getByText("usage.detail.speedEstimateTooFew"),
    ).toBeInTheDocument();
    expect(screen.queryByText("6.0s")).not.toBeInTheDocument();
  });

  it("says session logs have no timing data", () => {
    renderWith(
      log({
        dataSource: "codex_session",
        firstTokenMs: undefined,
        latencyMs: 0,
      }),
    );

    expect(
      screen.getByText("usage.detail.noTimingSession"),
    ).toBeInTheDocument();
    expect(screen.getByText("usage.detail.sourceSession")).toBeInTheDocument();
  });

  it("shows the error and a retry button when loading fails", () => {
    const refetch = vi.fn();
    useRequestDetailMock.mockReturnValue({
      data: undefined,
      isLoading: false,
      error: "ambiguous column name: created_at",
      refetch,
    });
    render(<RequestDetailPanel requestId="req_1" onClose={vi.fn()} />);

    expect(screen.getByText("usage.requestLoadFailed")).toBeInTheDocument();
    expect(
      screen.getByText("ambiguous column name: created_at"),
    ).toBeInTheDocument();
    expect(screen.queryByText("usage.requestNotFound")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "common.retry" }));
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  it.each([516, 1034, 1552, 2588, 517_999_998])(
    "highlights %i reasoning tokens in a non-GPT request detail",
    (reasoningOutputTokens) => {
      renderWith(log({ reasoningOutputTokens }));
      expect(
        screen.getByText("usage.reasoningOutputTokens"),
      ).toBeInTheDocument();
      expect(
        screen.getByText(reasoningOutputTokens.toLocaleString("en-US")),
      ).toHaveClass("text-danger-text");
    },
  );

  it.each([1, 515, 517, 1033, 1035, 1553])(
    "does not highlight normal reasoning count %i",
    (reasoningOutputTokens) => {
      renderWith(log({ reasoningOutputTokens }));
      expect(
        screen.getByText("usage.reasoningOutputTokens"),
      ).toBeInTheDocument();
      expect(
        screen.getByText(reasoningOutputTokens.toLocaleString("en-US")),
      ).not.toHaveClass("text-danger-text");
    },
  );

  it.each([undefined, 0])(
    "does not fabricate reasoning count %s",
    (reasoningOutputTokens) => {
      renderWith(log({ reasoningOutputTokens }));
      expect(
        screen.queryByText("usage.reasoningOutputTokens"),
      ).not.toBeInTheDocument();
    },
  );

  it.each([
    "session_log",
    "codex_session",
    "codex_sidebar",
    "gemini_session",
    "opencode_session",
    "pi_session",
    "grokbuild_session",
    "mcode_session",
    "local",
  ])(
    "shows a dash for %s latency and hides first-token/turn values",
    (dataSource) => {
      renderWith(log({ dataSource, durationMs: 5_208_631 }));
      const latency = screen.getByText("usage.latency").parentElement;
      expect(latency?.querySelector("dd")).toHaveTextContent(/^—$/);
      expect(
        screen.queryByText(/26\.4s|3\.2s|5208|firstToken|duration/i),
      ).not.toBeInTheDocument();
      if (dataSource === "codex_sidebar") {
        expect(screen.getByText("usage.sidebarProvider.label")).toHaveAttribute(
          "title",
          "codex_sidebar",
        );
      }
    },
  );

  it.each([undefined, "", "proxy"])(
    "keeps measured proxy speed and latency for source %s",
    (dataSource) => {
      renderWith(log({ dataSource, durationMs: 5_208_631 }));
      expect(screen.getByText("usage.speedValue:65")).toBeInTheDocument();
      expect(
        screen.queryByText(/^usage\.speedEstimatedValue:/),
      ).not.toBeInTheDocument();
      expect(screen.getByText("26.4s")).toBeInTheDocument();
      expect(
        screen.queryByText(/3\.2s|5208|firstToken|duration/i),
      ).not.toBeInTheDocument();
    },
  );

  it("says the request is not found only when the query returned nothing", () => {
    useRequestDetailMock.mockReturnValue({
      data: null,
      isLoading: false,
      error: null,
    });
    render(<RequestDetailPanel requestId="req_1" onClose={vi.fn()} />);

    expect(screen.getByText("usage.requestNotFound")).toBeInTheDocument();
    expect(screen.queryByText("usage.requestLoadFailed")).toBeNull();
  });
});
