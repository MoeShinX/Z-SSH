/** @vitest-environment jsdom */
//! 登录状态"不确定"时只提示、不阻断（R-2 第 3 节）。
//!
//! 用户的配置文件读不出来（编码坏了、没权限）时，不能把"读不到"当成"没登录"把人
//! 挡住——他自己的 CLI 可能早就登录好了。这条链路是：读不了 → `logged_in: null`
//! （后端）→ 面板显示提示但输入框不禁用、设置页显示"不确定"。

import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  aiAgentDiscover: vi.fn(async () => ({
    path: "C:\\tools\\claude\\claude.exe",
    version: "2.1.300 (Claude Code)",
    source: "tools_dir" as const,
    needs_shell: false,
    auto: true,
    too_old: false,
    skipped_older: null,
  })),
  aiAgentLogin: vi.fn(async () => ({
    logged_in: null,
    hint: "没能确认登录状态（settings.json 解析不了）。已经登录过就能直接用；不行就在终端里跑一次 `claude`",
  })),
}));

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "C:\\data", known_hosts: "C:\\data\\known_hosts" })),
  openDataDir: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: mocks.aiAgentDiscover,
    aiAgentLogin: mocks.aiAgentLogin,
    // 设置页挂载时会订阅下载进度（真实实现要 Tauri 的 IPC），一并挡掉，
    // 否则 jsdom 下会留下未处理的 rejection（退出码变 1）。
    onDownloadProgress: vi.fn(async () => () => {}),
    onAgentEvent: vi.fn(async () => () => {}),
    onApprovalRequest: vi.fn(async () => () => {}),
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

/** 打开设置页的 Claude Code 一节（默认停在"外观"）。 */
async function openAiSection() {
  const root = document.querySelector<HTMLElement>('[data-section="settings"]');
  if (!root) throw new Error("设置页没挂上");
  const button = [...root.querySelectorAll("button")].find(
    (node) => node.textContent?.trim() === "AI",
  );
  if (!button) throw new Error("设置页侧栏里没有 AI 按钮");
  button.click();
}

beforeEach(async () => {
  vi.clearAllMocks();
  stubBrowserApis();
  // 默认智能体选 Claude Code：登录状态不确定的分支只有在非 opencode 上才有。
  useSettings.setState({ ai_default_agent: "claude" });
});

describe("登录状态不确定（R-2 第 3 节）", () => {
  it("面板显示提示，但不禁用输入框", async () => {
    render(
      <AIChatPanel
        tabId="tab-1"
        sessions={[]}
        hostStates={{}}
        firstUse={false}
        onMarkUsed={() => {}}
        onOpenSettings={() => {}}
        onClose={() => {}}
      />,
    );

    await waitFor(() =>
      expect(document.querySelector('[data-section="login-unknown"]')).not.toBeNull(),
    );
    const notice = document.querySelector('[data-section="login-unknown"]');
    expect(notice?.textContent).toContain("没能确认登录状态");
    // 输入框仍然可用（不阻断）。
    const textarea = screen.getByPlaceholderText(/让 AI 在服务器上做什么/);
    expect((textarea as HTMLTextAreaElement).disabled).toBe(false);
  });

  it("设置页显示「不确定」，不是「未登录」", async () => {
    render(<SettingsPage />);
    await waitFor(() => expect(document.querySelector('[data-section="settings"]')).not.toBeNull());
    openAiSection();
    // jsdom 没有 innerText，用 textContent；限定在设置页里看。
    const settingsText = () =>
      document.querySelector<HTMLElement>('[data-section="settings"]')?.textContent ?? "";
    await waitFor(() => {
      expect(settingsText()).toContain("登录：不确定");
    });
    expect(settingsText()).not.toContain("登录：未登录");
  });
});
