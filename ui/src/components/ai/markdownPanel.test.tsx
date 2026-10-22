/** @vitest-environment jsdom */
//! AI 面板里的 Markdown 排版（AI-UX-1 第 3 项）：用户消息仍是纯文本、历史回看同样渲染、
//! 流式输出时已经写完的消息不重新解析。

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

const mdRenders = vi.hoisted(() => ({ texts: [] as string[] }));
vi.mock("react-markdown", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react-markdown")>();
  const Real = actual.default;
  return {
    ...actual,
    default: (props: React.ComponentProps<typeof Real>) => {
      mdRenders.texts.push(String(props.children));
      return <Real {...props} />;
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
  mdRenders.texts.length = 0;
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

const textDelta = (text: string) => ({ session_key: "ai-1", kind: "text_delta" as const, text });
const rendersOf = (text: string) => mdRenders.texts.filter((t) => t === text).length;

describe("AI 文字按 Markdown 排版", () => {
  it("AI 的回复渲染成标题 / 加粗 / 表格；用户自己发的消息仍是纯文本", async () => {
    mount();
    await startAndSend("**不是粗体** <b>x</b>");
    emit(textDelta("## 硬件配置\n\n**主机** web-1\n\n| 项 | 值 |\n| - | - |\n| CPU | 8 |"));
    await waitFor(() => expect(document.querySelector("h2")).not.toBeNull());
    expect(document.querySelector("h2")?.textContent).toBe("硬件配置");
    expect(document.querySelector("strong")?.textContent).toBe("主机");
    expect(document.querySelector("table")).not.toBeNull();
    // 用户消息：星号和尖括号原样显示，没有被解析
    expect(document.body.textContent).toContain("**不是粗体** <b>x</b>");
    expect(document.querySelectorAll("strong")).toHaveLength(1);
  });

  it("流式输出：已经写完的消息不重新解析，只有正在写的那条每帧重画", async () => {
    mount();
    await startAndSend("看看");
    const first = "第一段 **重点**";
    emit(textDelta(first));
    await waitFor(() => expect(document.body.textContent).toContain("第一段"));
    emit({ session_key: "ai-1", kind: "tool_start", call_id: "c1", tool: "zssh_run_command", input: { command: "ls" } });
    emit({ session_key: "ai-1", kind: "tool_end", call_id: "c1", tool: "zssh_run_command", output: "exit_code: 0", error: false });
    let second = "";
    for (const piece of ["第二", "段正在", "输出", "中……"]) {
      second += piece;
      emit(textDelta(piece));
      const now = second;
      await waitFor(() => expect(document.body.textContent).toContain(now));
    }
    // 第一条只解析了一次；第二条随着文字增长解析了多次
    expect(rendersOf(first)).toBe(1);
    expect(mdRenders.texts.filter((t) => second.startsWith(t) && t !== first).length).toBeGreaterThanOrEqual(3);
  });

  it("历史会话回看同样渲染 Markdown", async () => {
    mocks.api.aiHistoryLoad.mockResolvedValueOnce({
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
        { kind: "assistant", text: "## 结论\n\n磁盘还剩 **四成**" },
      ],
    });
    mount();
    await openList();
    fireEvent.click(await screen.findByText("看看磁盘"));
    await waitFor(() => expect(document.querySelector("h2")).not.toBeNull());
    expect(document.querySelector("h2")?.textContent).toBe("结论");
    expect(document.querySelector("strong")?.textContent).toBe("四成");
  });

  it("复制整条消息复制原始 Markdown", async () => {
    const writeText = vi.fn(async (_text: string) => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    mount();
    await startAndSend("看看");
    const raw = "## 标题\n\n**重点**";
    emit(textDelta(raw));
    const button = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-action="copy-message"]');
      if (!el) throw new Error("没有复制按钮");
      return el;
    });
    await act(async () => {
      fireEvent.click(button);
    });
    expect(writeText).toHaveBeenCalledWith(raw);
  });
});
