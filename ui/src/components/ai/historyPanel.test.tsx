/** @vitest-environment jsdom */
//! 面板里的历史会话（R11）：每轮结束保存、历史列表打开回看 / 删除、续接不上时的提示。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentEventPayload, HistoryRecord } from "../../api/agent";

const mocks = vi.hoisted(() => {
  const handlers: ((payload: AgentEventPayload) => void)[] = [];
  const approvalHandlers: ((request: unknown) => void)[] = [];
  return {
    handlers,
    approvalHandlers,
    settingsSet: vi.fn(async () => {}),
    api: {
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
      aiAgentCatalog: vi.fn(async () => ({ models: [], default: null })),
      aiAgentModels: vi.fn(async () => ({ models: [], default: null })),
      aiAgentStart: vi.fn(async (_agent: string, _path?: string, history?: string) => ({
        session_key: history ? "ai-resumed" : "ai-1",
        scope_id: "scope-1",
        agent_session_id: "ses_x",
        agent: "opencode" as const,
        resumed: false,
      })),
      aiAgentStop: vi.fn(async () => {}),
      aiAgentSend: vi.fn(async () => {}),
      aiAgentScopeSync: vi.fn(async () => {}),
      aiHistorySave: vi.fn(async () => null),
      aiHistoryList: vi.fn(async () => [
        {
          id: "h-1",
          agent: "opencode" as const,
          title: "看看磁盘",
          created_at: 1,
          updated_at: new Date(2020, 0, 2, 3, 4).getTime(),
          host_label: "web-1",
          message_count: 2,
          resumable: true,
        },
      ]),
      aiHistoryLoad: vi.fn(
        async (): Promise<HistoryRecord> => ({
          version: 1,
          id: "h-1",
          agent: "opencode",
          title: "看看磁盘",
          created_at: 1,
          updated_at: 2,
          host_label: "web-1",
          agent_session_id: "ses_old",
          messages: [
            { kind: "user", text: "看看磁盘" },
            { kind: "assistant", text: "磁盘还剩四成" },
          ],
        }),
      ),
      aiHistoryDelete: vi.fn(async () => {}),
      onAgentEvent: vi.fn(async (handler: (payload: AgentEventPayload) => void) => {
        handlers.push(handler);
        return () => {};
      }),
      onApprovalRequest: vi.fn(async (handler: (request: unknown) => void) => {
        approvalHandlers.push(handler);
        return () => {};
      }),
      aiApprovalAnswer: vi.fn(async () => {}),
    },
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return { ...actual, ...mocks.api };
});

import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { useSettings } from "../../stores/settings";

const NOTICE = "之前的上下文没有带给 AI，只供回看";

type Session = { sessionId: string; hostId: number | null; hostLabel: string; username: string };

/** 按钮 / 输入框能不能用（没装 jest-dom，直接看 disabled）。 */
function enabled(el: HTMLElement): boolean {
  return !(el as HTMLButtonElement).disabled;
}

const HOST_SESSION: Session = { sessionId: "s1", hostId: 3, hostLabel: "web-1", username: "" };

function mount(sessions: Session[] = [HOST_SESSION]) {
  return render(
    <AIChatPanel
      tabId="tab-1"
      sessions={sessions}
      hostStates={{}}
      firstUse={false}
      onMarkUsed={() => {}}
      onOpenSettings={() => {}}
      onClose={() => {}}
    />,
  );
}

function emit(payload: AgentEventPayload) {
  act(() => {
    for (const handler of mocks.handlers) handler(payload);
  });
}

type SaveCall = [
  string,
  string | null,
  { id: string; host_label: string | null; messages: unknown[] },
];

function saveCall(index: number): SaveCall {
  return mocks.api.aiHistorySave.mock.calls[index] as unknown as SaveCall;
}

async function startAndSend(text: string) {
  await waitFor(() => expect(enabled(screen.getByTitle("新对话"))).toBe(true));
  fireEvent.click(screen.getByTitle("新对话"));
  await waitFor(() => expect(mocks.api.aiAgentStart).toHaveBeenCalled());
  const box = document.querySelector("textarea");
  if (!box) throw new Error("没有输入框");
  await waitFor(() => expect(enabled(box)).toBe(true));
  fireEvent.change(box, { target: { value: text } });
  await waitFor(() => expect(enabled(screen.getByTitle("发送"))).toBe(true));
  fireEvent.click(screen.getByTitle("发送"));
  await waitFor(() => expect(mocks.api.aiAgentSend).toHaveBeenCalled());
}

async function openList(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByTitle("历史会话"));
  return waitFor(() => {
    const el = document.querySelector<HTMLElement>('[data-section="history-list"]');
    if (!el) throw new Error("列表没出来");
    return el;
  });
}

beforeEach(() => {
  clearCatalogCache();
  mocks.handlers.length = 0;
  mocks.approvalHandlers.length = 0;
  for (const fn of Object.values(mocks.api)) fn.mockClear();
  useSettings.setState({ ai_default_agent: "opencode" });
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => cleanup());

describe("每轮结束保存", () => {
  it("回合结束把用户消息和回复存进当前智能体的历史，带主机显示名", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit({ session_key: "ai-1", kind: "text_delta", text: "还剩四成" });
    expect(mocks.api.aiHistorySave).not.toHaveBeenCalled();
    emit({ session_key: "ai-1", kind: "turn_end" });
    await waitFor(() => expect(mocks.api.aiHistorySave).toHaveBeenCalledTimes(1));
    const [agent, key, draft] = saveCall(0);
    expect(agent).toBe("opencode");
    expect(key).toBe("ai-1");
    expect(draft.host_label).toBe("web-1");
    expect(draft.messages).toEqual([
      { kind: "user", text: "看看磁盘" },
      { kind: "assistant", text: "还剩四成" },
    ]);
    // 下一轮存进同一条历史（同一个 id）。
    emit({ session_key: "ai-1", kind: "turn_end" });
    await waitFor(() => expect(mocks.api.aiHistorySave).toHaveBeenCalledTimes(2));
    expect(saveCall(1)[2].id).toBe(draft.id);
  });

  it("快速连接的会话不存主机名（标签名里有地址）", async () => {
    mount([{ sessionId: "s1", hostId: null, hostLabel: "root@203.0.113.5:22", username: "" }]);
    await startAndSend("hi");
    emit({ session_key: "ai-1", kind: "turn_end" });
    await waitFor(() => expect(mocks.api.aiHistorySave).toHaveBeenCalled());
    expect(saveCall(0)[2].host_label).toBeNull();
  });
});

describe("AI 文字按帧合并显示", () => {
  it("一帧内的多段文字合成一次显示；回合结束存的是全文", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit({ session_key: "ai-1", kind: "text_delta", text: "还剩" });
    emit({ session_key: "ai-1", kind: "text_delta", text: "四" });
    emit({ session_key: "ai-1", kind: "text_delta", text: "成" });
    // 还没到下一帧：界面上还没有
    expect(document.body.textContent).not.toContain("还剩四成");
    await waitFor(() => expect(document.body.textContent).toContain("还剩四成"));
    emit({ session_key: "ai-1", kind: "text_delta", text: "，够用" });
    emit({ session_key: "ai-1", kind: "turn_end" });
    await waitFor(() => expect(mocks.api.aiHistorySave).toHaveBeenCalledTimes(1));
    expect(saveCall(0)[2].messages).toEqual([
      { kind: "user", text: "看看磁盘" },
      { kind: "assistant", text: "还剩四成，够用" },
    ]);
  });

  it("文字、工具、文字：顺序不乱（工具开始前先把攒的文字写进去）", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit({ session_key: "ai-1", kind: "text_delta", text: "我先看一下" });
    emit({ session_key: "ai-1", kind: "tool_start", call_id: "c1", tool: "zssh_list_hosts", input: {} });
    emit({ session_key: "ai-1", kind: "tool_end", call_id: "c1", tool: "zssh_list_hosts", output: "ok", error: false });
    emit({ session_key: "ai-1", kind: "text_delta", text: "看完了" });
    emit({ session_key: "ai-1", kind: "turn_end" });
    await waitFor(() => expect(mocks.api.aiHistorySave).toHaveBeenCalledTimes(1));
    const messages = saveCall(0)[2].messages as { kind: string; text?: string }[];
    expect(messages.map((m) => m.kind)).toEqual(["user", "assistant", "tool", "assistant"]);
    expect(messages[1].text).toBe("我先看一下");
    expect(messages[3].text).toBe("看完了");
  });

  it("命令卡片上的执行 / 拒绝：AI 还在出字时照样能点，点了回答对应的那条请求、卡片上的按钮消失", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit({ session_key: "ai-1", kind: "tool_start", call_id: "c1", tool: "zssh_run_command", input: { command: "df -h" } });
    emit({ session_key: "ai-1", kind: "tool_start", call_id: "c2", tool: "zssh_run_command", input: { command: "rm -rf /tmp/x" } });
    act(() => {
      for (const handler of mocks.approvalHandlers) {
        handler({ req_id: 41, scope: "scope-1", host_label: "web-1", command: "df -h", danger: "low", reasons: [], tool: "run_command" });
        handler({ req_id: 42, scope: "scope-1", host_label: "web-1", command: "rm -rf /tmp/x", danger: "high", reasons: [], tool: "run_command" });
      }
    });
    emit({ session_key: "ai-1", kind: "text_delta", text: "等你确认" });
    await waitFor(() => expect(screen.getAllByRole("button", { name: "执行" })).toHaveLength(2));
    // 拒绝第二条（rm），只回答 42
    fireEvent.click(screen.getAllByRole("button", { name: "拒绝" })[1]);
    await waitFor(() => expect(mocks.api.aiApprovalAnswer).toHaveBeenCalledWith(42, false));
    await waitFor(() => expect(screen.getAllByRole("button", { name: "执行" })).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "执行" }));
    await waitFor(() => expect(mocks.api.aiApprovalAnswer).toHaveBeenCalledWith(41, true));
    await waitFor(() => expect(screen.queryByRole("button", { name: "执行" })).toBeNull());
    expect(mocks.api.aiApprovalAnswer).toHaveBeenCalledTimes(2);
  });

  it("换新对话：上一条对话还没显示出来的文字丢掉，不串进新对话", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit({ session_key: "ai-1", kind: "text_delta", text: "上一条的尾巴" });
    fireEvent.click(screen.getByTitle("新对话"));
    await waitFor(() => expect(mocks.api.aiAgentStart).toHaveBeenCalledTimes(2));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(document.body.textContent).not.toContain("上一条的尾巴");
  });
});

describe("历史列表", () => {
  it("打开一条：回看消息、用历史 id 续接；没接上时提示上下文没带给 AI", async () => {
    mount();
    const list = await openList();
    await waitFor(() => expect(list.textContent).toContain("看看磁盘"));
    expect(list.textContent).toContain("web-1");
    expect(mocks.api.aiHistoryList).toHaveBeenCalledWith("opencode");

    fireEvent.click(screen.getByText("看看磁盘"));
    await waitFor(() =>
      expect(mocks.api.aiAgentStart).toHaveBeenCalledWith("opencode", expect.anything(), "h-1"),
    );
    await screen.findByText("磁盘还剩四成");
    await screen.findByText(NOTICE);
    expect(document.querySelector('[data-section="history-list"]')).toBeNull();
  });

  it("接上了就不提示；之后续接失败事件来了再提示", async () => {
    mocks.api.aiAgentStart.mockImplementationOnce(async () => ({
      session_key: "ai-resumed",
      scope_id: "scope-1",
      agent_session_id: "ses_old",
      agent: "opencode" as const,
      resumed: true,
    }));
    mount();
    await openList();
    fireEvent.click(await screen.findByText("看看磁盘"));
    await screen.findByText("磁盘还剩四成");
    await waitFor(() => expect(enabled(screen.getByTitle("新对话"))).toBe(true));
    expect(screen.queryByText(NOTICE)).toBeNull();
    emit({ session_key: "ai-resumed", kind: "resume_failed" });
    await screen.findByText(NOTICE);
  });

  it("每条可以删除", async () => {
    mount();
    await openList();
    const remove = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-action="delete-history"]');
      if (!el) throw new Error("没有删除按钮");
      return el;
    });
    fireEvent.click(remove);
    await waitFor(() =>
      expect(mocks.api.aiHistoryDelete).toHaveBeenCalledWith("opencode", "h-1"),
    );
    await waitFor(() => expect(document.querySelector("[data-history-id]")).toBeNull());
    expect(screen.getByText("还没有历史会话")).toBeTruthy();
  });
});
