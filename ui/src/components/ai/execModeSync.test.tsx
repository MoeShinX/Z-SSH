/** @vitest-environment jsdom */
//! 执行模式：面板和设置页读写**同一份**状态（R-2 0.1）。
//!
//! AI-4 验收发现的问题：面板自己 `settingsGet` / `settingsSet`，设置页读 store 里的
//! 快照——面板切成自动之后设置页还显示"手动"，在设置页动一下还会把模式改回去。
//! 这个开关决定命令执行前要不要确认，显示错了是安全问题。
//!
//! 用例就按验收要求的三个动作：面板切到自动 → 设置页显示"自动"；设置页改回手动 →
//! 面板显示"手动"。

import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  settingsSet: vi.fn(async () => {}),
  aiAgentDiscover: vi.fn(async () => ({
    path: "C:\\tools\\opencode\\opencode.exe",
    version: "1.19.1",
    source: "tools_dir" as const,
    needs_shell: false,
    auto: true,
    too_old: false,
    skipped_older: null,
  })),
  aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
  aiAgentGuard: vi.fn(async () => null),
  aiAgentDownloadInfo: vi.fn(async () => {
    throw new Error("测试里不下载");
  }),
  onDownloadProgress: vi.fn(async () => () => {}),
  onAgentEvent: vi.fn(async () => () => {}),
  onApprovalRequest: vi.fn(async () => () => {}),
}));

// 设置表：jsdom 下没有 Tauri。`settingsGet` 一律 null（等于"没存过"，走默认值），
// `settingsSet` 记下来给用例断言。
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
  dataDirInfo: vi.fn(async () => ({ data_dir: "C:\\data", known_hosts: "C:\\data\\known_hosts" })),
  openDataDir: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: mocks.aiAgentDiscover,
    aiAgentLogin: mocks.aiAgentLogin,
    aiAgentGuard: mocks.aiAgentGuard,
    aiAgentDownloadInfo: mocks.aiAgentDownloadInfo,
    onDownloadProgress: mocks.onDownloadProgress,
    onAgentEvent: mocks.onAgentEvent,
    onApprovalRequest: mocks.onApprovalRequest,
  };
});

vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.1.0-test") }));

vi.mock("../../api/vault", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/vault")>();
  return {
    ...actual,
    defaultProxyGet: vi.fn(async () => ({ enabled: false, url: "" })),
    defaultProxySet: vi.fn(async () => {}),
  };
});

import AIChatPanel from "./AIChatPanel";
import SettingsPage from "../settings/SettingsPage";
import { useSettings } from "../../stores/settings";

/** Radix Select 在 jsdom 下要的几个浏览器 API（打开下拉框用）。 */
function stubBrowserApis() {
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
  Element.prototype.scrollIntoView = () => {};
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}

/** 面板 + 设置页一起挂上（同一个 store，两个界面）。 */
function mountBoth() {
  const view = render(
    <div>
      <AIChatPanel
        tabId="tab-1"
        sessions={[]}
        hostStates={{}}
        firstUse={false}
        onMarkUsed={() => {}}
        onOpenSettings={() => {}}
        onClose={() => {}}
      />
      <SettingsPage />
    </div>,
  );
  return view;
}

/** 面板顶部的执行模式那行（面板是一排三个按钮，设置页是下拉框）。 */
function panelExecRow(): HTMLElement {
  const row = document.querySelector<HTMLElement>('[data-section="exec-mode"]');
  if (!row) throw new Error("面板里没有执行模式那一行");
  return row;
}

/** 面板上当前选中的那档。 */
function panelModeText(): string {
  return panelExecRow().querySelector('[role="radio"][aria-checked="true"]')?.textContent ?? "";
}

/** 在一个 Radix 下拉框里选一项：先按下触发器展开，再点选项。 */
async function choose(trigger: HTMLElement, label: string) {
  await act(async () => {
    // Radix 的触发器按 `pointerType === "mouse"` 判断要不要展开（jsdom 没有真的指针）。
    fireEvent.pointerDown(trigger, {
      button: 0,
      ctrlKey: false,
      pointerId: 1,
      pointerType: "mouse",
    });
  });
  const option = await screen.findByRole("option", { name: label });
  await act(async () => {
    fireEvent.click(option);
  });
}

async function chooseInPanel(label: string) {
  const button = [...panelExecRow().querySelectorAll<HTMLElement>('[role="radio"]')].find(
    (node) => node.textContent === label,
  );
  if (!button) throw new Error(`面板里没有「${label}」这一档`);
  await act(async () => {
    fireEvent.click(button);
  });
}

/** 打开设置页的 AI 一节（默认停在"外观"）。 */
async function openAiSection() {
  const root = document.querySelector<HTMLElement>('[data-section="settings"]');
  if (!root) throw new Error("设置页没挂上");
  // 侧栏里那个"AI"按钮；用精确文本，别撞见面板里的别的按钮。
  const button = [...root.querySelectorAll("button")].find(
    (node) => node.textContent?.trim() === "AI",
  );
  if (!button) throw new Error("设置页侧栏里没有 AI 按钮");
  await act(async () => {
    fireEvent.click(button);
  });
}

function settingsExecTrigger(): HTMLElement {
  const node = document.querySelector<HTMLElement>('[data-section="settings-exec-mode"]');
  if (!node) throw new Error("设置页里没有执行模式下拉框");
  return node;
}

function settingsModeText(): string {
  return settingsExecTrigger().textContent ?? "";
}

async function chooseInSettings(label: string) {
  await choose(settingsExecTrigger(), label);
}

beforeEach(async () => {
  vi.clearAllMocks();
  stubBrowserApis();
  // 每个用例从"手动"这个默认值开始（store 是模块级单例）。
  await act(async () => {
    await useSettings.getState().update("ai_exec_mode", "manual");
  });
  useSettings.setState({ ai_exec_mode: "manual" });
});

describe("执行模式：面板和设置页共用一份状态（R-2 0.1）", () => {
  it("面板切到全部放行后，设置页也显示「全部放行」", async () => {
    mountBoth();
    await waitFor(() => expect(panelModeText()).toContain("逐条确认"));
    await openAiSection();
    expect(settingsModeText()).toContain("逐条确认");

    await chooseInPanel("全部放行");

    expect(panelModeText()).toContain("全部放行");
    expect(settingsModeText()).toContain("全部放行");
    // 落到后端（面板和设置页写的是同一个键）。
    expect(mocks.settingsSet).toHaveBeenCalledWith("ai_exec_mode", JSON.stringify("auto"));
  });

  it("在设置页选只读自动，面板跟着变", async () => {
    mountBoth();
    await chooseInPanel("全部放行");
    await openAiSection();

    await chooseInSettings("只读自动");

    await waitFor(() => expect(settingsModeText()).toContain("只读自动"));
    expect(panelModeText()).toContain("只读自动");
    expect(mocks.settingsSet).toHaveBeenCalledWith("ai_exec_mode", JSON.stringify("readonly"));
  });
});
