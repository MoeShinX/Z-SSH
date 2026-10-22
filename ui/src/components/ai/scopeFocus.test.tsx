/** @vitest-environment jsdom */
//! AI-SCOPE-1：目标按主机去重、对话中切焦点保留范围、卡片标主机名 + 查看终端、
//! 后台标签的执行活动事件。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentEventPayload, ScopeTargetChange } from "../../api/agent";

const mocks = vi.hoisted(() => {
  const handlers: ((payload: AgentEventPayload) => void)[] = [];
  return {
    handlers,
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
      onApprovalRequest: vi.fn(async () => () => {}),
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

type Session = { sessionId: string; hostId: number | null; hostLabel: string; username: string };

const WEB: Session = { sessionId: "s-web", hostId: 3, hostLabel: "web-1", username: "root" };
const DB: Session = { sessionId: "s-db", hostId: 4, hostLabel: "db-1", username: "root" };

const enabled = (el: HTMLElement) => !(el as HTMLButtonElement).disabled;

interface Extra {
  onViewTarget?: (target: { sessionId: string | null; hostId: number | null }) => void;
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
    await waitFor(() => expect(syncState().targets.get("s-web")?.pinned).toBe(true));
    const { targets } = syncState();
    expect([...targets.keys()].sort()).toEqual(["s-db", "s-web"]);
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
    expect(removedEver).not.toContain("host-9");
  });
});

describe("切焦点不影响作用范围", () => {
  it("对话进行中切焦点：原来的当前转成可 × 的标签并留在范围里，且没有被移出过", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await send("看看负载");
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.get("s-db")?.current).toBe(true));
    const { targets, removedEver } = syncState();
    expect(targets.get("s-web")).toMatchObject({ pinned: true, current: false });
    expect(removedEver).not.toContain("s-web");
    const chip = document.querySelector<HTMLElement>('[data-section="pinned-target"]');
    expect(chip?.textContent).toContain("web-1");
    expect(chip?.dataset.retained).toBe("true");

    // 用户 × 掉才移出
    fireEvent.click(chip?.querySelector("button") as HTMLElement);
    await waitFor(() => expect(syncState().targets.has("s-web")).toBe(false));
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(0);
  });

  it("切回去：合并成一个，目标 ID 不变", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    await send("看看负载");
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.has("s-web")).toBe(true));
    view.rerender(panel([WEB]));
    await waitFor(() => expect(syncState().targets.get("s-web")?.current).toBe(true));
    const { targets, removedEver } = syncState();
    expect(targets.get("s-web")?.pinned).toBe(true);
    // 对话还在进行，刚才的 db-1 同样被保留
    expect(targets.get("s-db")).toMatchObject({ pinned: true, current: false });
    expect(removedEver).not.toContain("s-web");
    expect(document.querySelectorAll('[data-section="pinned-target"]')).toHaveLength(1);
  });

  it("对话还没开始（没发过消息）：当前跟着焦点走，不累积", async () => {
    const view = render(panel([WEB]));
    await startConversation();
    view.rerender(panel([DB]));
    await waitFor(() => expect(syncState().targets.has("s-db")).toBe(true));
    expect(syncState().targets.has("s-web")).toBe(false);
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
    await waitFor(() => expect(syncState().targets.has("s-db")).toBe(true));
    expect(mocks.api.aiAgentStop).not.toHaveBeenCalled();
  });
});

describe("工具卡片：执行所在的主机和查看终端", () => {
  async function expandGroup() {
    const toggle = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-action="toggle-tool-group"]');
      if (!el) throw new Error("没有折叠行");
      return el;
    });
    fireEvent.click(toggle);
  }

  it("目标多于一个：卡片标主机名，点查看终端交给 App 切过去", async () => {
    const onViewTarget = vi.fn();
    render(panel([WEB, DB], { onViewTarget }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "s-db", command: "uptime" }));
    emit(callEnd("c1"));
    await expandGroup();
    const label = document.querySelector('[data-section="call-target"]');
    expect(label?.textContent).toBe("db-1");
    fireEvent.click(document.querySelector('[data-action="view-terminal"]') as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ sessionId: "s-db", hostId: 4 });
  });

  it("文件工具同样带主机名和查看终端；host-<id> 目标没有会话时只给主机 id", async () => {
    const onViewTarget = vi.fn();
    render(panel([WEB], { onViewTarget }));
    await startConversation();
    await pickHost("cache-1");
    await send("检查");
    emit(callStart("c1", { target_id: "host-9", path: "/etc/hosts" }, "zssh_read_file"));
    emit(callEnd("c1", "zssh_read_file"));
    await expandGroup();
    expect(document.querySelector('[data-section="call-target"]')?.textContent).toBe("cache-1");
    fireEvent.click(document.querySelector('[data-action="view-terminal"]') as HTMLElement);
    expect(onViewTarget).toHaveBeenCalledWith({ sessionId: null, hostId: 9 });
  });

  it("只有一个目标：不显示主机名（避免啰嗦），查看终端仍在", async () => {
    const onViewTarget = vi.fn();
    render(panel([WEB], { onViewTarget }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { command: "uptime" }));
    emit(callEnd("c1"));
    await expandGroup();
    expect(document.querySelector('[data-section="call-target"]')).toBeNull();
    expect(document.querySelector('[data-action="view-terminal"]')).not.toBeNull();
  });

  it("切走后关掉的会话，旧卡片仍认得主机名", async () => {
    const view = render(panel([WEB, DB]));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "s-web", command: "uptime" }));
    emit(callEnd("c1"));
    view.rerender(panel([DB]));
    await expandGroup();
    // 只剩 db-1 一个目标但保留了 web-1，所以是两个目标：标主机名
    expect(document.querySelector('[data-section="call-target"]')?.textContent).toBe("web-1");
  });
});

describe("后台标签的执行活动事件", () => {
  it("run_command 开始 / 结束带上目标；回合结束清空", async () => {
    const events: AiActivityEvent[] = [];
    render(panel([WEB, DB], { onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { target_id: "s-db", command: "uptime" }));
    emit(callStart("c2", { command: "df -h" }));
    emit(callEnd("c1"));
    // 没写 target_id 的是当时的当前
    expect(events).toEqual([
      { kind: "start", callId: "c1", targetId: "s-db" },
      { kind: "start", callId: "c2", targetId: "s-web" },
      { kind: "end", callId: "c1" },
    ]);
    emit({ session_key: "ai-1", kind: "turn_end" } as AgentEventPayload);
    expect(events[events.length - 1]).toEqual({ kind: "reset" });
  });

  it("文件工具不算在终端里执行", async () => {
    const events: AiActivityEvent[] = [];
    render(panel([WEB], { onAiActivity: (e) => events.push(e) }));
    await startConversation();
    await send("检查");
    emit(callStart("c1", { path: "/etc/hosts" }, "zssh_read_file"));
    emit(callEnd("c1", "zssh_read_file"));
    expect(events).toEqual([]);
  });
});
