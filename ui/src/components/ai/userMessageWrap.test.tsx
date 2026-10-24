/** @vitest-environment jsdom */
//! 用户消息气泡要能换行：没有换行类时，长命令 / 网址这类不可断行内容会把气泡撑得比面板还宽
//! （面板窄时被裁掉、拉宽才显示得全——AI 面板调宽度时反馈的正是这个）。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  aiAgentDiscover: vi.fn(async (agent: string) =>
    agent === "opencode"
      ? {
          path: "C:/tools/opencode/opencode.exe",
          version: "1.19.1",
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
  onAgentEvent: vi.fn(async () => () => {}),
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

import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { useSettings } from "../../stores/settings";

beforeEach(() => {
  clearCatalogCache();
  vi.clearAllMocks();
  useSettings.setState({ ai_default_agent: "opencode" });
});

afterEach(cleanup);

describe("用户消息换行", () => {
  it("气泡带 pre-wrap 和 break-words：长网址这类内容能折行", async () => {
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
    fireEvent.change(input, {
      target: { value: "看看这个 https://example.com/a/very/long/path/that/never/breaks/without-css-help" },
    });
    await act(async () => fireEvent.keyDown(input, { key: "Enter" }));

    // 用户气泡（蓝底、右对齐）的类名里要有换行两件套
    const bubble = Array.from(document.querySelectorAll("div")).find(
      (el) => el.className.includes("bg-primary") && el.className.includes("rounded-lg"),
    );
    expect(bubble).toBeTruthy();
    expect(bubble!.className).toContain("whitespace-pre-wrap");
    expect(bubble!.className).toContain("break-words");
    expect(bubble!.textContent).toContain("https://example.com/a/very/long/path");
  });
});
