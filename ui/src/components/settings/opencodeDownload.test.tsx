/** @vitest-environment jsdom */
//! 设置页"下载 opencode"完成后的状态刷新（R11 用户反馈）。
//!
//! 反馈的现象：PATH 上是旧版 → 设置页显示"版本过低"；点"下载 opencode"下完后界面
//! 还是旧状态，要手动点"重新检测"才显示新版本。这里钉住：
//! - 下载成功 → 自动重新检测一次（和"重新检测"按钮同一个入口），显示新版本；
//! - 下载失败 → 不重新检测，显示错误；
//! - 下载中按钮禁用，连点也只发起一次；确认框里取消后按钮恢复。

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentInstall, ReleaseInfo } from "../../api/agent";
import { downloadAndRecheck, errorText } from "./opencodeDownload";

const OLD: AgentInstall = {
  path: "C:/bin/opencode.exe",
  version: "1.2.0",
  source: "path",
  needs_shell: false,
  auto: true,
  too_old: true,
  skipped_older: null,
};

const NEW: AgentInstall = {
  path: "C:/data/tools/opencode/opencode.exe",
  version: "1.19.3",
  source: "tools_dir",
  needs_shell: false,
  auto: true,
  too_old: false,
  skipped_older: { version: "1.2.0", source: "path" },
};

const RELEASE: ReleaseInfo = {
  version: "1.19.3",
  asset_name: "opencode-windows-x64.zip",
  size: 10 * 1024 * 1024,
  sha256: "00",
  download_url: "https://example.invalid/opencode.zip",
  page_url: "https://example.invalid/release",
};

const mocks = vi.hoisted(() => ({
  aiAgentDiscover: vi.fn(),
  aiAgentDownload: vi.fn(),
  aiAgentDownloadInfo: vi.fn(),
  aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
  aiAgentGuard: vi.fn(async () => null),
}));

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "C:/data", known_hosts: "C:/data/known_hosts" })),
  openDataDir: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: mocks.aiAgentDiscover,
    aiAgentDownload: mocks.aiAgentDownload,
    aiAgentDownloadInfo: mocks.aiAgentDownloadInfo,
    aiAgentLogin: mocks.aiAgentLogin,
    aiAgentGuard: mocks.aiAgentGuard,
    // 真实实现要 Tauri 的 IPC，jsdom 下会留下未处理的 rejection。
    onDownloadProgress: vi.fn(async () => () => {}),
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

import { AISection } from "./SettingsPage";

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

/** opencode 那一块的 discover 调用次数（codex / claude 的不算）。 */
function opencodeDiscoverCalls(): number {
  return mocks.aiAgentDiscover.mock.calls.filter(([agent]) => agent === "opencode").length;
}

/** 让 opencode 的发现结果按"PATH 上旧版 → 下载后新版"切换。 */
let opencodeInstalled: AgentInstall = OLD;

describe("downloadAndRecheck", () => {
  it("下载成功后调用一次重新检测，返回安装信息", async () => {
    const recheck = vi.fn();
    const outcome = await downloadAndRecheck(async () => NEW, recheck);
    expect(outcome).toEqual({ ok: true, install: NEW });
    expect(recheck).toHaveBeenCalledTimes(1);
  });

  it("下载失败不重新检测，错误文字原样带回（Error 只取 message）", async () => {
    const recheck = vi.fn();
    const fromString = await downloadAndRecheck(() => Promise.reject("SHA-256 不一致"), recheck);
    expect(fromString).toEqual({ ok: false, error: "SHA-256 不一致" });
    const fromError = await downloadAndRecheck(
      () => Promise.reject(new Error("网络超时")),
      recheck,
    );
    expect(fromError).toEqual({ ok: false, error: "网络超时" });
    expect(recheck).not.toHaveBeenCalled();
  });

  it("errorText：字符串和 Error 都转成给人看的文字", () => {
    expect(errorText("x")).toBe("x");
    expect(errorText(new Error("y"))).toBe("y");
  });
});

describe("设置页 AI：下载 opencode 后自动刷新状态", () => {
  beforeEach(() => {
    stubBrowserApis();
    opencodeInstalled = OLD;
    mocks.aiAgentDiscover.mockReset();
    mocks.aiAgentDiscover.mockImplementation(async (agent: string) =>
      agent === "opencode" ? opencodeInstalled : null,
    );
    mocks.aiAgentDownload.mockReset();
    mocks.aiAgentDownloadInfo.mockReset();
    mocks.aiAgentDownloadInfo.mockResolvedValue(RELEASE);
  });

  afterEach(() => {
    cleanup();
  });

  it("下载成功：不用点重新检测，界面直接显示新版本", async () => {
    mocks.aiAgentDownload.mockImplementation(async () => {
      opencodeInstalled = NEW;
      return NEW;
    });
    render(<AISection />);
    expect(await screen.findByText("版本过低，建议升级", { exact: false })).toBeTruthy();
    expect(opencodeDiscoverCalls()).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "下载 opencode" }));
    fireEvent.click(await screen.findByRole("button", { name: "开始下载" }));

    expect(await screen.findByText("当前：1.19.3（已下载）")).toBeTruthy();
    expect(opencodeDiscoverCalls()).toBe(2);
    expect(screen.queryByText("版本过低，建议升级", { exact: false })).toBeNull();
    expect(screen.getByRole("button", { name: "下载 opencode" })).toHaveProperty("disabled", false);
  });

  it("下载失败：不重新检测，显示错误，按钮恢复", async () => {
    mocks.aiAgentDownload.mockRejectedValue("下载的文件 SHA-256 和发布页公布的不一致");
    render(<AISection />);
    await screen.findByText("版本过低，建议升级", { exact: false });

    fireEvent.click(screen.getByRole("button", { name: "下载 opencode" }));
    fireEvent.click(await screen.findByRole("button", { name: "开始下载" }));

    expect(
      await screen.findByText("下载失败：下载的文件 SHA-256 和发布页公布的不一致"),
    ).toBeTruthy();
    expect(opencodeDiscoverCalls()).toBe(1);
    expect(screen.getByText("当前：1.2.0（PATH）")).toBeTruthy();
    expect(screen.getByRole("button", { name: "下载 opencode" })).toHaveProperty("disabled", false);
  });

  it("下载中按钮禁用；连点两下只查一次下载信息", async () => {
    let finish: (install: AgentInstall) => void = () => {};
    mocks.aiAgentDownload.mockImplementation(
      () =>
        new Promise<AgentInstall>((resolve) => {
          finish = resolve;
        }),
    );
    render(<AISection />);
    await screen.findByText("版本过低，建议升级", { exact: false });

    const button = screen.getByRole("button", { name: "下载 opencode" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(mocks.aiAgentDownloadInfo).toHaveBeenCalledTimes(1);

    fireEvent.click(await screen.findByRole("button", { name: "开始下载" }));
    const busy = await screen.findByRole("button", { name: "下载中…" });
    expect(busy).toHaveProperty("disabled", true);
    fireEvent.click(busy);
    expect(mocks.aiAgentDownload).toHaveBeenCalledTimes(1);

    opencodeInstalled = NEW;
    finish(NEW);
    expect(await screen.findByText("当前：1.19.3（已下载）")).toBeTruthy();
  });

  it("确认框里取消：按钮恢复成可点，不会一直显示下载中", async () => {
    render(<AISection />);
    await screen.findByText("版本过低，建议升级", { exact: false });

    fireEvent.click(screen.getByRole("button", { name: "下载 opencode" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "下载 opencode" })).toHaveProperty(
        "disabled",
        false,
      ),
    );
    expect(mocks.aiAgentDownload).not.toHaveBeenCalled();
  });
});
