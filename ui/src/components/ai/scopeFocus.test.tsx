/** @vitest-environment jsdom */
//! AI-SCOPE-1、AI-SCOPE-2：一台主机一个逻辑目标（绑定会话跟着焦点走）、对话中切焦点保留范围、
//! 卡片标主机名 + 查看终端、后台标签的执行活动事件。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AgentEventPayload,
  ApprovalRequest,
  ExecTargetReport,
  ScopeTargetChange,
} from "../../api/agent";

const mocks = vi.hoisted(() => {
  const handlers: ((payload: AgentEventPayload) => void)[] = [];
  const execHandlers: ((report: ExecTargetReport) => void)[] = [];
  const approvalHandlers: ((request: ApprovalRequest) => void)[] = [];
  return {
    handlers,
    execHandlers,
    approvalHandlers,
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
      aiAgentStart: vi.fn(async () => ({
        session_key: "ai-1",
        scope_id: "scope-1",
        agent_session_id: "ses_x",
        agent: "opencode" as const,
        resumed: false,
      })),
      aiAgentStop: vi.fn(async () => {}),
      aiAgentSend: vi.fn(async () => {}),
      aiAgentScopeSync: vi.fn(
        async (_key: string, _added: ScopeTargetChange[], _removed: string[]) => {},
      ),
      aiHistorySave: vi.fn(async () => null),
      aiHistoryList: vi.fn(async () => []),
      aiHostPicker: vi.fn(async () => [
        { host_id: 3, name: "web-1", username: "root" },
        { host_id: 9, name: "cache-1", username: "ops" },
      ]),
      onAgentEvent: vi.fn(async (handler: (payload: AgentEventPayload) => void) => {
        handlers.push(handler);
        return () => {};
      }),
      onApprovalRequest: vi.fn(async (handler: (request: ApprovalRequest) => void) => {
        approvalHandlers.push(handler);
        return () => {};
      }),
      onExecTarget: vi.fn(async (handler: (report: ExecTargetReport) => void) => {
        execHandlers.push(handler);
        return () => {};
      }),
      aiApprovalAnswer: vi.fn(async () => {}),
    },
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return { ...actual, ...mocks.api };
});

import AIChatPanel, { type AiActivityEvent } from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { useSettings } from "../../stores/settings";

type Session = {
  sessionId: string;
  hostId: number | null;
  hostLabel: string;
  username: string;
  focused?: boolean;
};

const WEB: Session = { sessionId: "s-web", hostId: 3, hostLabel: "web-1", username: "root" };
const DB: Session = { sessionId: "s-db", hostId: 4, hostLabel: "db-1", username: "root" };
const WEB2: Session = { ...WEB, sessionId: "s-web2" };
const QUICK: Session = { sessionId: "s-q", hostId: null, hostLabel: "临时", username: "u" };

const enabled = (el: HTMLElement) => !(el as HTMLButtonElement).disabled;

interface Extra {
  openSessionIds?: ReadonlySet<string>;
  onViewTarget?: (target: { targetId: string; sessionId: string | null }) => void;
  onAiActivity?: (event: AiActivityEvent) => void;
}

function panel(sessions: Session[], extra: Extra = {}) {
  return (
    <AIChatPanel
      tabId="tab-1"
      sessions={sessions}
      hostStates={{}}
      firstUse={false}
      onMarkUsed={() => {}}
      onOpenSettings={() => {}}
      onClose={() => {}}
      {...extra}
    />
  );
}

async function startConversation() {
  await waitFor(() => expect(enabled(screen.getByTitle("新对话"))).toBe(true));
  fireEvent.click(screen.getByTitle("新对话"));
  await waitFor(() => expect(mocks.api.aiAgentStart).toHaveBeenCalled());
  await waitFor(() => expect(mocks.api.aiAgentScopeSync).toHaveBeenCalled());
}

async function send(text: string) {
  const box = document.querySelector("textarea");
  if (!box) throw new Error("没有输入框");
  await waitFor(() => expect(enabled(box)).toBe(true));
  fireEvent.change(box, { target: { value: text } });
  await waitFor(() => expect(enabled(screen.getByTitle("发送"))).toBe(true));
  fireEvent.click(screen.getByTitle("发送"));
  await waitFor(() => expect(mocks.api.aiAgentSend).toHaveBeenCalled());
}

/** 通过 @ 选择弹窗加一台主机。 */
async function pickHost(name: string) {
  const box = document.querySelector("textarea");
  if (!box) throw new Error("没有输入框");
  fireEvent.change(box, { target: { value: "@" } });
  const item = await screen.findByText(name, { selector: "span" });
  fireEvent.click(item.closest("button") as HTMLElement);
  fireEvent.change(box, { target: { value: "" } });
}

/** 最近一次同步后后端应该有的目标。 */
function syncState() {
  const targets = new Map<string, ScopeTargetChange>();
  const removedEver: string[] = [];
  for (const [, added, removed] of mocks.api.aiAgentScopeSync.mock.calls) {
    for (const t of added) targets.set(t.target_id, t);
    for (const id of removed) {
      targets.delete(id);
      removedEver.push(id);
    }
  }
  return { targets, removedEver };
}

function emit(payload: AgentEventPayload) {
  act(() => {
    for (const handler of mocks.handlers) handler(payload);
  });
}

/** 执行层的回报（默认是 run_command 落在 web-1 的 s-web 上）。 */
function emitReport(over: Partial<ExecTargetReport> = {}) {
  const report: ExecTargetReport = {
    scope: "scope-1",
    tool: "run_command",
    key: "",
    target_id: "host-3",
    session_id: "s-web",
    host_label: "web-1",
    ...over,
  };
  act(() => {
    for (const handler of mocks.execHandlers) handler(report);
  });
}

function emitApproval(request: ApprovalRequest) {
  act(() => {
    for (const handler of mocks.approvalHandlers) handler(request);
  });
}

const callStart = (id: string, input: Record<string, unknown>, tool = "zssh_run_command") => ({
  session_key: "ai-1",
  kind: "tool_start" as const,
  call_id: id,
  tool,
  input,
});
const callEnd = (id: string, tool = "zssh_run_command") => ({
  session_key: "ai-1",
  kind: "tool_end" as const,
  call_id: id,
  tool,
  output: "exit_code: 0\nlocation: terminal",
  error: false,
});

beforeEach(() => {
  clearCatalogCache();
  mocks.handlers.length = 0;
  mocks.execHandlers.length = 0;
  mocks.approvalHandlers.length = 0;
  for (const fn of Object.values(mocks.api)) fn.mockClear();
  useSettings.setState({ ai_default_agent: "opencode" });
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => cleanup());

describe("按主机去重", () => {
  it("当前会话的主机又被 @：只发一个目标且 pinned，界面上也只有一个标签", async () => {
    render(panel([WEB, DB]));
    await startConversation();
    await pickHost("web-1");
    await waitFor(() => expect(syncState().targets.get("host-3")?.pinned).toBe(true));
    const { targets } = syncState();
    expect([...targets.keys()].sort()).toEqual(["host-3", "host-4"]);
    expect(targets.get("host-3")).toMatchObject({ current: true, session_id: "s-web" });
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(0);
    expect(screen.getAllByText(/web-1/).length).toBeGreaterThan(0);
  });

  it("@ 一台没打开的主机：host-<id> 目标，切到它的标签（会话出现）后合并成一个且 ID 不变", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await pickHost("cache-1");
    await waitFor(() => expect(syncState().targets.has("host-9")).toBe(true));
    expect(syncState().targets.get("host-9")).toMatchObject({ pinned: true, current: false });

    const cache: Session = { sessionId: "s-cache", hostId: 9, hostLabel: "cache-1", username: "ops" };
    view.rerender(panel([cache]));
    await waitFor(() => expect(syncState().targets.get("host-9")?.current).toBe(true));
    const { targets, removedEver } = syncState();
    expect(targets.has("s-cache")).toBe(false);
    expect(targets.get("host-9")).toMatchObject({ session_id: "s-cache", pinned: true });
    expect(removedEver).not.toContain("host-9");
  });
});

describe("切焦点不影响作用范围", () => {
  it("对话进行中切焦点：原来的当前转成可 × 的标签并留在范围里，且没有被移出过", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await send("看看负载");
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.get("host-4")?.current).toBe(true));
    const { targets, removedEver } = syncState();
    expect(targets.get("host-3")).toMatchObject({ pinned: true, current: false, session_id: "s-web" });
    expect(removedEver).not.toContain("host-3");
    const chip = document.querySelector<HTMLElement>('[data-section="pinned-target"]');
    expect(chip?.textContent).toContain("web-1");
    expect(chip?.dataset.retained).toBe("true");

    // 用户 × 掉才移出
    fireEvent.click(chip?.querySelector("button") as HTMLElement);
    await waitFor(() => expect(syncState().targets.has("host-3")).toBe(false));
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(0);
  });

  it("切回去：合并成一个，目标 ID 不变", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await send("看看负载");
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.has("host-3")).toBe(true));
    view.rerender(panel([WEB]));
    await waitFor(() => expect(syncState().targets.get("host-3")?.current).toBe(true));
    const { targets, removedEver } = syncState();
    expect(targets.get("host-3")?.pinned).toBe(true);
    // 对话还在进行，刚才的 db-1 同样被保留
    expect(targets.get("host-4")).toMatchObject({ pinned: true, current: false });
    expect(removedEver).not.toContain("host-3");
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(1);
  });

  it("对话还没开始（没发过消息）：当前跟着焦点走，不累积", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.has("host-4")).toBe(true));
    expect(syncState().targets.has("host-3")).toBe(false);
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(0);
  });

  it("切标签（面板收到新的 tabId）不结束对话", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await send("看看负载");
    view.rerender(
      <AIChatPanel
        tabId="tab-2"
        sessions={[DB]}
        hostStates={{}}
        firstUse={false}
        onMarkUsed={() => {}}
        onOpenSettings={() => {}}
        onClose={() => {}}
      />,
    );
    await waitFor(() => expect(syncState().targets.has("host-4")).toBe(true));
    expect(mocks.api.aiAgentStop).not.toHaveBeenCalled();
  });
});

describe("同一台主机多个终端：一个逻辑目标（AI-SCOPE-2）", () => {
  it("两个标签同一台主机，对话中来回切换：始终一个目标、ID 不变、绑定跟着焦点走、从未被移出", async () => {
    const open = new Set([WEB.sessionId, WEB2.sessionId]);
    const view = render(panel([{ ...WEB, focused: true }], { openSessionIds: open }));
    await startConversation();
    await send("看看负载");
    await waitFor(() => expect(syncState().targets.get("host-3")?.session_id).toBe("s-web"));
    for (const [session, expected] of [
      [WEB2, "s-web2"],
      [WEB, "s-web"],
      [WEB2, "s-web2"],
    ] as const) {
      view.rerender(panel([{ ...session, focused: true }], { openSessionIds: open }));
      await waitFor(() => expect(syncState().targets.get("host-3")?.session_id).toBe(expected));
      expect([...syncState().targets.keys()]).toEqual(["host-3"]);
    }
    expect(syncState().removedEver).toEqual([]);
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(0);
  });

  it("先 @ 再切换同一台主机的标签：仍是一个目标，pinned，绑定跟着走", async () => {
    const open = new Set([WEB.sessionId, WEB2.sessionId]);
    const view = render(panel([{ ...WEB, focused: true }], { openSessionIds: open }));
    await startConversation();
    await pickHost("web-1");
    await send("看看负载");
    await waitFor(() => expect(syncState().targets.get("host-3")?.pinned).toBe(true));
    view.rerender(panel([{ ...WEB2, focused: true }], { openSessionIds: open }));
    await waitFor(() => expect(syncState().targets.get("host-3")?.session_id).toBe("s-web2"));
    expect([...syncState().targets.keys()]).toEqual(["host-3"]);
    expect(syncState().targets.get("host-3")?.pinned).toBe(true);
    expect(syncState().removedEver).toEqual([]);
  });

  it("分屏两个窗格同一台主机：一个目标，焦点换窗格绑定跟着换", async () => {
    const open = new Set([WEB.sessionId, WEB2.sessionId]);
    const view = render(
      panel([{ ...WEB, focused: true }, { ...WEB2, focused: false }], { openSessionIds: open }),
    );
    await startConversation();
    await waitFor(() => expect(syncState().targets.get("host-3")?.session_id).toBe("s-web"));
    view.rerender(
      panel([{ ...WEB, focused: false }, { ...WEB2, focused: true }], { openSessionIds: open }),
    );
    await waitFor(() => expect(syncState().targets.get("host-3")?.session_id).toBe("s-web2"));
    expect([...syncState().targets.keys()]).toEqual(["host-3"]);
    expect(syncState().targets.get("host-3")?.current).toBe(true);
  });

  it("切走后再回来，这台主机不在当前标签时绑定最近获得焦点的会话", async () => {
    const open = new Set([WEB.sessionId, WEB2.sessionId, DB.sessionId]);
    const view = render(panel([{ ...WEB2, focused: true }], { openSessionIds: open }));
    await startConversation();
    await send("看看负载");
    view.rerender(panel([{ ...DB, focused: true }], { openSessionIds: open }));
    await waitFor(() => expect(syncState().targets.get("host-4")?.current).toBe(true));
    expect(syncState().targets.get("host-3")).toMatchObject({
      current: false,
      pinned: true,
      session_id: "s-web2",
    });
  });

  it("快速连接的会话不受影响：目标 ID 仍是会话 ID", async () => {
    render(panel([QUICK, WEB]));
    await startConversation();
    const { targets } = syncState();
    expect([...targets.keys()].sort()).toEqual(["host-3", "s-q"]);
    expect(targets.get("s-q")).toMatchObject({ current: true, session_id: "s-q" });
  });
});

describe("工具卡片：执行层回报的执行位置和查看终端（AI-SCOPE-2）", () => {
  async function expandGroup() {
    const toggle = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-action="toggle-tool-group"]');
      if (!el) throw new Error("没有折叠行");
      return el;
    });
    fireEvent.click(toggle);
  }
  const targetLabel = () => document.querySelector('[data-section="call-target"]')?.textContent;
  const viewButton = () => document.querySelector<HTMLElement>('[data-action="view-terminal"]');

  it("目标多于一个：卡片标回报里的主机名，点查看终端按回报的会话交给 App", async () => {
    const onViewTarget = vi.fn();
    render(panel([WEB, DB], { onViewTarget }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "host-4", command: "uptime" }));
    emitReport({ key: "uptime", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emit(callEnd("c1"));
    await expandGroup();
    expect(targetLabel()).toBe("db-1");
    fireEvent.click(viewButton() as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ targetId: "host-4", sessionId: "s-db" });
  });

  it("回报比工具开始事件先到、或者比工具结束事件还晚到，都能对上卡片", async () => {
    render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    // 先到
    emitReport({ key: "df -h", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emit(callStart("c1", { command: "df -h" }));
    emit(callEnd("c1"));
    // 晚到
    emit(callStart("c2", { command: "free -m" }));
    emit(callEnd("c2"));
    emitReport({ key: "free -m", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    await expandGroup();
    const labels = [...document.querySelectorAll('[data-section="call-target"]')].map(
      (el) => el.textContent,
    );
    expect(labels).toEqual(["db-1", "web-1"]);
    expect(document.querySelector('[data-section="call-target-unknown"]')).toBeNull();
  });

  it("两条同工具的调用各认各的回报（按命令对）", async () => {
    render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { command: "a" }));
    emit(callStart("c2", { command: "b" }));
    emitReport({ key: "b", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emitReport({ key: "a", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    emit(callEnd("c1"));
    emit(callEnd("c2"));
    await expandGroup();
    const labels = [...document.querySelectorAll('[data-section="call-target"]')].map(
      (el) => el.textContent,
    );
    expect(labels).toEqual(["web-1", "db-1"]);
  });

  it("两台主机同时执行同一条命令，回报乱序：主机名、查看终端、活动点各自对应", async () => {
    const onViewTarget = vi.fn();
    const events: AiActivityEvent[] = [];
    render(panel([WEB, DB], { onViewTarget, onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "host-3", command: "uptime" }));
    emit(callStart("c2", { target_id: "host-4", command: "uptime" }));
    // host-4 的回报先到，host-3 的后到
    emitReport({ key: "uptime", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emitReport({ key: "uptime", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    emit(callEnd("c1"));
    emit(callEnd("c2"));
    await expandGroup();
    const labels = [...document.querySelectorAll('[data-section="call-target"]')].map(
      (el) => el.textContent,
    );
    expect(labels).toEqual(["web-1", "db-1"]);
    const buttons = [...document.querySelectorAll<HTMLElement>('[data-action="view-terminal"]')];
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);
    expect(onViewTarget.mock.calls).toEqual([
      [{ targetId: "host-3", sessionId: "s-web" }],
      [{ targetId: "host-4", sessionId: "s-db" }],
    ]);
    expect(events.filter((e) => e.kind === "start")).toEqual([
      { kind: "start", callId: "c2", targetId: "host-4", sessionId: "s-db" },
      { kind: "start", callId: "c1", targetId: "host-3", sessionId: "s-web" },
    ]);
  });

  it("回报先于两条卡片到达：各自按目标认领，不串", async () => {
    render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    emitReport({ key: "uptime", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emitReport({ key: "uptime", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    emit(callStart("c1", { target_id: "host-3", command: "uptime" }));
    emit(callStart("c2", { target_id: "host-4", command: "uptime" }));
    emit(callEnd("c1"));
    emit(callEnd("c2"));
    await expandGroup();
    const labels = [...document.querySelectorAll('[data-section="call-target"]')].map(
      (el) => el.textContent,
    );
    expect(labels).toEqual(["web-1", "db-1"]);
  });

  it("没有当前会话、两个目标都不是当前：模型省略 target_id，卡片显示后端选中的主机且能查看终端", async () => {
    const onViewTarget = vi.fn();
    render(panel([], { onViewTarget }));
    await startConversation();
    await pickHost("web-1");
    await pickHost("cache-1");
    await send("检查");
    await waitFor(() => expect(syncState().targets.size).toBe(2));
    expect([...syncState().targets.values()].every((t) => !t.current)).toBe(true);
    emit(callStart("c1", { command: "uptime" }));
    // 后端按它自己的规则选中了列表第一项
    emitReport({ key: "uptime", target_id: "host-9", session_id: null, host_label: "cache-1" });
    emit(callEnd("c1"));
    await expandGroup();
    expect(targetLabel()).toBe("cache-1");
    fireEvent.click(viewButton() as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ targetId: "host-9", sessionId: null });
  });

  it("只有一个目标但它不是当前：也标出主机名", async () => {
    render(panel([]));
    await startConversation();
    await pickHost("web-1");
    await send("检查");
    emit(callStart("c1", { command: "uptime" }));
    emitReport({ key: "uptime", target_id: "host-3", session_id: null, host_label: "web-1" });
    emit(callEnd("c1"));
    await expandGroup();
    expect(targetLabel()).toBe("web-1");
  });

  it("只有一个当前目标：不显示主机名（避免啰嗦），查看终端仍在", async () => {
    render(panel([WEB], { onViewTarget: vi.fn() }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { command: "uptime" }));
    emitReport({ key: "uptime", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    emit(callEnd("c1"));
    await expandGroup();
    expect(targetLabel()).toBeUndefined();
    expect(viewButton()).not.toBeNull();
  });

  it("文件工具同样用回报；读终端没有路径也能对上", async () => {
    const onViewTarget = vi.fn();
    render(panel([WEB, DB], { onViewTarget }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { path: "/etc/hosts" }, "zssh_read_file"));
    emitReport({
      tool: "read_file",
      key: "/etc/hosts",
      target_id: "host-4",
      session_id: "s-db",
      host_label: "db-1",
    });
    emit(callEnd("c1", "zssh_read_file"));
    emit(callStart("c2", {}, "zssh_read_terminal"));
    emitReport({
      tool: "read_terminal",
      key: "",
      target_id: "host-3",
      session_id: "s-web",
      host_label: "web-1",
    });
    emit(callEnd("c2", "zssh_read_terminal"));
    await expandGroup();
    const labels = [...document.querySelectorAll('[data-section="call-target"]')].map(
      (el) => el.textContent,
    );
    expect(labels).toEqual(["db-1", "web-1"]);
  });

  it("同一台主机多个会话：「查看终端」交出的是回报里执行的那个会话", async () => {
    const onViewTarget = vi.fn();
    render(panel([{ ...WEB, focused: true }, WEB2, DB], { onViewTarget }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "host-3", command: "uptime" }));
    emitReport({ key: "uptime", target_id: "host-3", session_id: "s-web2", host_label: "web-1" });
    emit(callEnd("c1"));
    await expandGroup();
    fireEvent.click(viewButton() as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ targetId: "host-3", sessionId: "s-web2" });
  });

  it("调用结束了也没有回报：写明「执行目标未确定」，不静默省略", async () => {
    render(panel([WEB, DB], { onViewTarget: vi.fn() }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { command: "uptime" }));
    emit({
      session_key: "ai-1",
      kind: "tool_end",
      call_id: "c1",
      tool: "zssh_run_command",
      output: "目标不存在或不属于本次对话的作用范围",
      error: true,
    });
    await expandGroup();
    expect(document.querySelector('[data-section="call-target-unknown"]')?.textContent).toBe(
      "执行目标未确定",
    );
    expect(viewButton()).toBeNull();
  });

  it("别的对话的回报不会对到这个对话的卡片上", async () => {
    render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { command: "uptime" }));
    emitReport({ scope: "other-scope", key: "uptime", target_id: "host-4", host_label: "db-1" });
    emit(callEnd("c1"));
    await expandGroup();
    expect(targetLabel()).toBeUndefined();
    expect(document.querySelector('[data-section="call-target-unknown"]')).not.toBeNull();
  });

  it("审批卡片用审批事件里的执行位置：没有对应工具卡片时单独显示主机和查看终端", async () => {
    const onViewTarget = vi.fn();
    render(panel([], { onViewTarget }));
    await startConversation();
    await pickHost("web-1");
    await send("检查");
    emitApproval({
      req_id: 5,
      scope: "scope-1",
      host_label: "web-1",
      command: "systemctl restart nginx",
      danger: "safe",
      reasons: [],
      target_id: "host-3",
      session_id: "s-web2",
      tool: "run_command",
    });
    await waitFor(() => expect(targetLabel()).toBe("web-1"));
    fireEvent.click(viewButton() as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ targetId: "host-3", sessionId: "s-web2" });
  });

  it("切走后关掉的会话，旧卡片仍用回报里的主机名", async () => {
    const view = render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "host-3", command: "uptime" }));
    emitReport({ key: "uptime", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    emit(callEnd("c1"));
    view.rerender(panel([DB]));
    await expandGroup();
    expect(targetLabel()).toBe("web-1");
  });
});

describe("后台标签的执行活动事件", () => {
  it("run_command 的回报到了才开始：带回报里的目标和会话；结束、回合结束照旧", async () => {
    const events: AiActivityEvent[] = [];
    render(panel([WEB, DB], { onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "host-4", command: "uptime" }));
    expect(events).toEqual([]);
    emitReport({ key: "uptime", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    emit(callStart("c2", { command: "df -h" }));
    emitReport({ key: "df -h", target_id: "host-3", session_id: "s-web2", host_label: "web-1" });
    emit(callEnd("c1"));
    expect(events).toEqual([
      { kind: "start", callId: "c1", targetId: "host-4", sessionId: "s-db" },
      { kind: "start", callId: "c2", targetId: "host-3", sessionId: "s-web2" },
      { kind: "end", callId: "c1" },
    ]);
    emit({ session_key: "ai-1", kind: "turn_end" } as AgentEventPayload);
    expect(events[events.length - 1]).toEqual({ kind: "reset" });
  });

  it("回报先到也能点亮；没有回报的调用不点亮（只有结束事件）", async () => {
    const events: AiActivityEvent[] = [];
    render(panel([WEB], { onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emitReport({ key: "ls", target_id: "host-3", session_id: null, host_label: "web-1" });
    emit(callStart("c1", { command: "ls" }));
    emit(callStart("c2", { command: "pwd" }));
    emit(callEnd("c2"));
    expect(events).toEqual([
      { kind: "start", callId: "c1", targetId: "host-3", sessionId: null },
      { kind: "end", callId: "c2" },
    ]);
  });

  it("文件工具不算在终端里执行", async () => {
    const events: AiActivityEvent[] = [];
    render(panel([WEB], { onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { path: "/etc/hosts" }, "zssh_read_file"));
    emitReport({ tool: "read_file", key: "/etc/hosts" });
    emit(callEnd("c1", "zssh_read_file"));
    expect(events).toEqual([]);
  });
});
