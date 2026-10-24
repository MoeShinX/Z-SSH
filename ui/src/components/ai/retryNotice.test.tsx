/** @vitest-environment jsdom */
//! 智能体自动重试（额度用完 / 限流 / 过载）时，「思考中」旁边给提示，
//! 出字或回合结束后提示消失；重试超限停掉这一轮时，错误提示出现、「思考中」收起。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  agentHandler: null as ((event: Record<string, unknown>) => void) | null,
  toastError: vi.fn(),
  aiAgentDiscover: vi.fn(async (agent: string) =>
    agent === "opencode"
      ? {
          path: "C:\tools\opencode\opencode.exe",
          version: "1.19.4",
          source: "tools_dir" as const,
          needs_shell: false,
          auto: true,
          too_old: false,
          skipped_older: null,
        }
      : null,
  ),
  aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
  aiAgentCatalog: vi.fn(async () => ({ models: [], default: null, default_label: null })),
  aiAgentModels: vi.fn(async () => ({ models: [], default: null, default_label: null })),
  aiAgentStart: vi.fn(async () => ({ session_key: "k1", scope_id: "s1", resumed: false })),
  aiAgentSend: vi.fn(async () => {}),
  aiAgentScopeSync: vi.fn(async () => {}),
  aiAgentStop: vi.fn(async () => {}),
  onAgentEvent: vi.fn(async (handler: (event: Record<string, unknown>) => void) => {
    mocks.agentHandler = handler;
    return () => {};
  }),
  onApprovalRequest: vi.fn(async () => () => {}),
  onTargetConnect: vi.fn(async () => () => {}),
}));

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return { ...actual, ...mocks };
});

vi.mock("../ui/toast", () => ({
  toast: {
    error: mocks.toastError,
    success: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  },
}));

import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { retryText } from "./ThinkingIndicator";
import { useSettings } from "../../stores/settings";

beforeEach(() => {
  clearCatalogCache();
  vi.clearAllMocks();
  useSettings.setState({ ai_default_agent: "opencode" });
});

afterEach(() => cleanup());

async function startTurn() {
  render(
    <AIChatPanel
      tabId="tab-1"
      sessions={[{ sessionId: "sess-1", hostId: 1, hostLabel: "web-1", username: "root" }]}
      hostStates={{}}
      firstUse={false}
      onMarkUsed={() => {}}
      onOpenSettings={() => {}}
      onClose={() => {}}
    />,
  );
  const input = screen.getByPlaceholderText(/让 AI 在服务器上做什么/);
  await waitFor(() => expect((input as HTMLTextAreaElement).disabled).toBe(false));
  fireEvent.change(input, { target: { value: "你好" } });
  const send = screen.getByTitle("发送");
  await waitFor(() => expect((send as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(send);
  await waitFor(() => expect(mocks.aiAgentSend).toHaveBeenCalledTimes(1));
  expect((await screen.findByRole("status")).textContent).toContain("思考中");
}

const emit = async (event: Record<string, unknown>) =>
  act(async () => {
    mocks.agentHandler?.({ session_key: "k1", ...event });
  });

const retrying = (attempt: number, reason: string, next: number | null = null) => ({
  kind: "retrying",
  attempt,
  reason,
  next_in_secs: next,
});

const noticeText = () => document.querySelector('[data-section="ai-retrying"]')?.textContent ?? null;

describe("重试提示文字", () => {
  it("带次数、原因；有下次时间才写「约 X 秒后重试」", () => {
    expect(retryText({ attempt: 2, reason: "请求太频繁，被服务商限流了", nextInSecs: 8 })).toBe(
      "遇到问题，正在重试（第 2 次，最多 3 次）：请求太频繁，被服务商限流了，约 8 秒后重试",
    );
    expect(retryText({ attempt: 1, reason: "服务商那边暂时忙不过来", nextInSecs: null })).toBe(
      "遇到问题，正在重试（第 1 次，最多 3 次）：服务商那边暂时忙不过来",
    );
    expect(retryText({ attempt: 1, reason: "x", nextInSecs: 0 })).not.toContain("后重试");
    expect(retryText({ attempt: 1, reason: "x", nextInSecs: 125 })).toContain("约 3 分钟后重试");
    expect(retryText({ attempt: 9, reason: "", nextInSecs: null })).toBe(
      "遇到问题，正在重试（第 3 次，最多 3 次）",
    );
  });
});

describe("AI 面板：自动重试提示", () => {
  it("收到重试事件显示提示；更新次数；出字后消失", async () => {
    await startTurn();
    expect(noticeText()).toBeNull();
    await emit(retrying(1, "免费额度用完了（opencode 提示：Free usage exceeded）", 4));
    expect(noticeText()).toContain("第 1 次，最多 3 次");
    expect(noticeText()).toContain("免费额度用完了");
    expect(noticeText()).toContain("约 4 秒后重试");
    await emit(retrying(2, "免费额度用完了"));
    expect(noticeText()).toContain("第 2 次");
    expect(noticeText()).not.toContain("后重试");
    // 重试成功，开始出字：提示和「思考中」一起收起
    await emit({ kind: "text_delta", text: "好了" });
    await waitFor(() => expect(document.querySelector('[data-section="ai-thinking"]')).toBeNull());
    expect(noticeText()).toBeNull();
  });

  it("重试超限被停掉：错误提示出现，「思考中」和重试提示都消失", async () => {
    await startTurn();
    await emit(retrying(3, "服务商那边暂时忙不过来"));
    expect(noticeText()).toContain("第 3 次");
    const message =
      "重试了 3 次仍然失败：服务商那边暂时忙不过来。已停止这一轮，可以稍后再试或换一个模型。";
    await emit({ kind: "error", message });
    await emit({ kind: "turn_end" });
    expect(mocks.toastError).toHaveBeenCalledWith(message);
    expect(noticeText()).toBeNull();
    expect(document.querySelector('[data-section="ai-thinking"]')).toBeNull();
  });

  it("回合正常结束时重试提示也不留着", async () => {
    await startTurn();
    await emit(retrying(1, "请求太频繁，被服务商限流了", 2));
    expect(noticeText()).not.toBeNull();
    await emit({ kind: "turn_end" });
    expect(noticeText()).toBeNull();
    expect(document.querySelector('[data-section="ai-thinking"]')).toBeNull();
  });
});
