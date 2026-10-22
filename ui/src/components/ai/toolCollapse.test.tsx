/** @vitest-environment jsdom */
//! AI 面板里执行完的工具调用折成一行（AI-UX-1）：默认收起、点开看完整卡片、失败用警示色、
//! 等待批准的不折叠、正在执行显示一行进行中、历史回看同样折叠。

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

const call = (id: string, command: string) => ({
  session_key: "ai-1",
  kind: "tool_start" as const,
  call_id: id,
  tool: "zssh_run_command",
  input: { command },
});
const done = (id: string, output = "exit_code: 0\nlocation: terminal") => ({
  session_key: "ai-1",
  kind: "tool_end" as const,
  call_id: id,
  tool: "zssh_run_command",
  output,
  error: false,
});

const groups = () => Array.from(document.querySelectorAll<HTMLElement>('[data-section="tool-group"]'));
const toggle = (group: HTMLElement) =>
  fireEvent.click(group.querySelector('[data-action="toggle-tool-group"]') as HTMLElement);

describe("执行完的工具调用折叠", () => {
  it("一轮里两条成功命令：一行「执行了 2 条命令」，默认收起，点开看到两张卡片，再点收起", async () => {
    mount();
    await startAndSend("看看磁盘");
    emit(call("c1", "df -h"));
    emit(done("c1"));
    emit(call("c2", "free -m"));
    emit(done("c2"));
    await waitFor(() => expect(groups()).toHaveLength(1));
    const group = groups()[0];
    expect(group.textContent).toContain("执行了 2 条命令");
    expect(group.dataset.failed).toBeUndefined();
    // 收起：没有卡片，命令全文也看不到
    expect(document.body.textContent).not.toContain("df -h");
    expect(document.body.textContent).not.toContain("已在终端里执行");
    toggle(group);
    expect(document.body.textContent).toContain("df -h");
    expect(document.body.textContent).toContain("free -m");
    expect(group.textContent).toContain("退出码 0");
    expect(group.textContent?.match(/已在终端里执行/g)).toHaveLength(2);
    toggle(group);
    expect(document.body.textContent).not.toContain("df -h");
  });

  it("命令和文件工具混在一起：一行写明各是几个", async () => {
    mount();
    await startAndSend("看看");
    emit(call("c1", "ls"));
    emit(done("c1"));
    emit({ session_key: "ai-1", kind: "tool_start", call_id: "f1", tool: "zssh_read_file", input: { path: "/etc/hosts" } });
    emit({ session_key: "ai-1", kind: "tool_end", call_id: "f1", tool: "zssh_read_file", output: "x", error: false });
    await waitFor(() => expect(groups()).toHaveLength(1));
    expect(groups()[0].textContent).toContain("执行了 1 条命令、读了 1 个文件");
  });

  it("其中一条退出码非 0：这行用警示色并写明失败，仍默认收起", async () => {
    mount();
    await startAndSend("看看");
    emit(call("c1", "true"));
    emit(done("c1"));
    emit(call("c2", "false"));
    emit(done("c2", "exit_code: 1"));
    await waitFor(() => expect(groups()).toHaveLength(1));
    const group = groups()[0];
    expect(group.dataset.failed).toBe("true");
    expect(group.textContent).toContain("其中 1 条失败");
    expect(group.querySelector("button")?.className).toContain("amber");
    expect(document.body.textContent).not.toContain("exit_code");
    expect(document.body.textContent).not.toContain("退出码");
  });

  it("工具报错、被拒绝也算失败", async () => {
    mount();
    await startAndSend("看看");
    emit(call("c1", "ls"));
    emit({ ...done("c1", "用户拒绝了这条命令"), error: true });
    emit(call("c2", "ls /x"));
    emit({ ...done("c2", "boom"), error: true });
    await waitFor(() => expect(groups()).toHaveLength(1));
    expect(groups()[0].textContent).toContain("其中 2 条失败");
  });

  it("文字消息把前后两批工具调用隔成两行", async () => {
    mount();
    await startAndSend("看看");
    emit(call("c1", "ls"));
    emit(done("c1"));
    emit({ session_key: "ai-1", kind: "text_delta", text: "先看到这里" });
    emit(call("c2", "pwd"));
    emit(done("c2"));
    await waitFor(() => expect(groups()).toHaveLength(2));
    expect(document.body.textContent).toContain("先看到这里");
  });

  it("等待批准的命令完整显示、不折叠；批准并执行完以后并入折叠行", async () => {
    mount();
    await startAndSend("清理");
    emit(call("c1", "ls"));
    emit(done("c1"));
    emit(call("c2", "rm -rf /tmp/x"));
    act(() => {
      for (const handler of mocks.approvalHandlers) {
        handler({ req_id: 7, scope: "scope-1", host_label: "web-1", command: "rm -rf /tmp/x", danger: "high", reasons: ["删除文件"], tool: "run_command" });
      }
    });
    await waitFor(() => expect(screen.getByRole("button", { name: "执行" })).toBeTruthy());
    expect(document.body.textContent).toContain("rm -rf /tmp/x");
    expect(document.body.textContent).toContain("删除文件");
    // 前面已经执行完的那条折成一行
    expect(groups()).toHaveLength(1);
    expect(groups()[0].textContent).toContain("执行了 1 条命令");

    fireEvent.click(screen.getByRole("button", { name: "执行" }));
    await waitFor(() => expect(mocks.api.aiApprovalAnswer).toHaveBeenCalledWith(7, true));
    emit(done("c2"));
    await waitFor(() => expect(groups()[0].textContent).toContain("执行了 2 条命令"));
    expect(groups()).toHaveLength(1);
    expect(document.body.textContent).not.toContain("rm -rf /tmp/x");
  });

  it("正在执行：显示一行「正在执行：命令前 60 个字符…」，执行完并入折叠行", async () => {
    mount();
    await startAndSend("看看");
    const long = "echo " + "a".repeat(100);
    emit(call("c1", long));
    const row = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-section="tool-progress"]');
      if (!el) throw new Error("没有进行中的行");
      return el;
    });
    expect(row.textContent).toBe("正在执行：" + long.slice(0, 60) + "…");
    expect(groups()).toHaveLength(0);
    emit(done("c1"));
    await waitFor(() => expect(groups()).toHaveLength(1));
    expect(document.querySelector('[data-section="tool-progress"]')).toBeNull();
  });
});

describe("历史会话回看同样折叠", () => {
  it("旧记录里连续的工具调用折成一行，失败的警示", async () => {
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
        { kind: "tool", tool: "zssh_run_command", input: { command: "df -h" }, output: "exit_code: 0", status: "done" },
        { kind: "tool", tool: "zssh_run_command", input: { command: "bad" }, output: "exit_code: 2", status: "done" },
        { kind: "assistant", text: "磁盘还剩四成" },
      ],
    });
    mount();
    await openList();
    fireEvent.click(await screen.findByText("看看磁盘"));
    await screen.findByText("磁盘还剩四成");
    expect(groups()).toHaveLength(1);
    expect(groups()[0].textContent).toContain("执行了 2 条命令");
    expect(groups()[0].textContent).toContain("其中 1 条失败");
    expect(document.body.textContent).not.toContain("df -h");
    toggle(groups()[0]);
    expect(document.body.textContent).toContain("df -h");
  });
});
