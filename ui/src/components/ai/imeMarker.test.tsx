/** @vitest-environment jsdom */
//! AI 对话输入框是输入法的「跟随区」：带 data-ime="native" 标记；
//! 英文区（这里用一个普通输入框代替）→ AI 输入框 → 恢复原来的中英文状态。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ime = vi.hoisted(() => ({ calls: [] as boolean[] }));

vi.mock("../../api/ime", () => ({
  imeTerminalFocus: vi.fn(async (focused: boolean) => {
    ime.calls.push(focused);
    return focused;
  }),
  onImeRestorePending: vi.fn(async () => () => {}),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));
vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: vi.fn(async () => null),
    aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
    aiAgentCatalog: vi.fn(async () => ({ models: [], default: null, default_label: null })),
    aiAgentModels: vi.fn(async () => ({ models: [], default: null, default_label: null })),
    aiAgentScopeSync: vi.fn(async () => {}),
    aiAgentStop: vi.fn(async () => {}),
    onAgentEvent: vi.fn(async () => () => {}),
    onApprovalRequest: vi.fn(async () => () => {}),
    onTargetConnect: vi.fn(async () => () => {}),
  };
});

import { useSettings } from "../../stores/settings";
import { useImeDefaultEnglish } from "../../terminal/useImeDefaultEnglish";
import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";

function Probe() {
  useImeDefaultEnglish();
  return null;
}

beforeEach(() => {
  vi.useFakeTimers();
  clearCatalogCache();
  ime.calls.length = 0;
  useSettings.setState({ terminal_ime_english: true, loaded: true });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("AI 对话输入框是输入法的跟随区", () => {
  it("输入框带 data-ime=native 且是唯一一个；只有终端切英文，搜索框等跟随用户", async () => {
    render(
      <>
        <Probe />
        <input aria-label="search" />
        <div className="xterm">
          <textarea aria-label="terminal" data-ime="terminal" />
        </div>
        <AIChatPanel
          tabId="tab-1"
          sessions={[{ sessionId: "sess-1", hostId: 1, hostLabel: "web-1", username: "root" }]}
          hostStates={{}}
          firstUse={false}
          onMarkUsed={() => {}}
          onOpenSettings={() => {}}
          onClose={() => {}}
        />
      </>,
    );
    const ai = screen.getByPlaceholderText(/让 AI 在服务器上做什么/) as HTMLTextAreaElement;
    expect(ai.getAttribute("data-ime")).toBe("native");
    expect(document.querySelectorAll('[data-ime="native"]')).toHaveLength(1);

    const search = screen.getByLabelText("search");
    await act(async () => {
      search.focus();
      await vi.advanceTimersByTimeAsync(300);
    });
    expect(ime.calls).toEqual([]);
    const terminal = screen.getByLabelText("terminal");
    await act(async () => {
      terminal.focus();
      await vi.advanceTimersByTimeAsync(300);
    });
    expect(ime.calls).toEqual([true]);
    await act(async () => {
      // 面板刚挂载时输入框可能还是禁用的（等智能体探测），聚焦前放开
      ai.disabled = false;
      ai.focus();
      await vi.advanceTimersByTimeAsync(300);
    });
    expect(ime.calls).toEqual([true, false]);
  });
});
