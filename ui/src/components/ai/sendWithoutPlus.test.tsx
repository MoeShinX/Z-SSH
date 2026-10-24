/** @vitest-environment jsdom */
//! 刚打开面板还没有对话时，输入后直接发送：先自动开对话再发出去，不用先点"+"。
//! 发出去以后、AI 还没出字之前，消息底部显示"思考中"。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  agentHandler: null as ((event: Record<string, unknown>) => void) | null,
  aiAgentDiscover: vi.fn(async (agent: string) =>
    agent === "opencode"
      ? {
          path: "C:\\tools\\opencode\\opencode.exe",
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

import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { useSettings } from "../../stores/settings";

beforeEach(() => {
  clearCatalogCache();
  vi.clearAllMocks();
  useSettings.setState({ ai_default_agent: "opencode" });
});

afterEach(() => cleanup());

describe("不点 + 直接发消息", () => {
  it("第一条消息先自动开对话，再发出去", async () => {
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
    expect(mocks.aiAgentStart).toHaveBeenCalledTimes(1);
    const [key, text] = mocks.aiAgentSend.mock.calls[0] as unknown[];
    expect(key).toBe("k1");
    expect(text).toBe("你好");
    expect(await screen.findByText("你好")).toBeTruthy();

    // AI 还没出字：显示"思考中"
    expect((await screen.findByRole("status")).textContent).toContain("思考中");
    // 开始出字就收起
    await act(async () => {
      mocks.agentHandler?.({ session_key: "k1", kind: "text_delta", text: "在的" });
    });
    await waitFor(() => expect(document.querySelector('[data-section="ai-thinking"]')).toBeNull());
  });
});

describe("输入法组合期间的 Enter 不发送", () => {
  it("选候选字的 Enter 只交给输入法；组合结束后的 Enter 才发送", async () => {
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

    // 正在打中文：isComposing / keyCode 229 的 Enter 是选字，不发送、内容保留
    fireEvent.change(input, { target: { value: "看下磁盘" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(mocks.aiAgentStart).not.toHaveBeenCalled();
    expect(mocks.aiAgentSend).not.toHaveBeenCalled();
    expect((input as HTMLTextAreaElement).value).toBe("看下磁盘");

    // 组合结束后的 Enter 正常发送
    fireEvent.keyDown(input, { key: "Enter", isComposing: false });
    await waitFor(() => expect(mocks.aiAgentSend).toHaveBeenCalledTimes(1));
  });
});
