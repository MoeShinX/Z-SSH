/** @vitest-environment jsdom */
//! 同步页里 OneDrive 的部分（任务卡 S-4）：存储选择、登录按钮的可用条件（编进程序的 client ID /
//! “高级”里自己填的）、登录中提示和取消、登录成功后的摘要、登录失效、断开和撤销授权的提示。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ConnectionProbe, SyncStatusView } from "../../../api/sync";
import { makeStatus } from "../../../state/syncFixtures";
import { useSync } from "../../../stores/sync";
import SyncStatusBar from "../../sync/SyncStatusBar";
import SyncSection from "./SyncSection";

const api = vi.hoisted(() => ({
  syncStatus: vi.fn(),
  syncConfigSave: vi.fn(),
  syncTestConnection: vi.fn(),
  syncSetPassword: vi.fn(),
  syncRememberPassword: vi.fn(),
  syncChangePassword: vi.fn(),
  syncNow: vi.fn(),
  syncAutoSet: vi.fn(),
  syncConflictsList: vi.fn(),
  syncConflictUseOther: vi.fn(),
  syncConflictDismiss: vi.fn(),
  syncGithubCreateGist: vi.fn(),
  syncOnedriveLogin: vi.fn(),
  syncOnedriveLoginCancel: vi.fn(),
  syncOnedriveDisconnect: vi.fn(),
  syncOnedriveOpenRevokePage: vi.fn(),
  syncBackupsList: vi.fn(),
  syncBackupRestore: vi.fn(),
  syncBackupKeepSet: vi.fn(),
  syncResetBaseline: vi.fn(),
  onSyncStatus: vi.fn(),
}));
vi.mock("../../../api/sync", () => api);
vi.mock("../../../stores/vault", () => ({
  useVault: { getState: () => ({ load: vi.fn().mockResolvedValue(undefined) }) },
}));

const GOOD_PROBE: ConnectionProbe = {
  read: { ok: true, message: "已连接，云端还没有同步数据" },
  write: { ok: true, message: "可以写入" },
  concurrency: {
    mode: "conditional",
    notes: ["服务器返回了 ETag", "对象已存在时 If-None-Match 被正确拒绝"],
  },
  has_existing_data: false,
};

const ACCOUNT = "Test User";
const CUSTOM_ID = "11111111-2222-3333-4444-555555555555";

function onedriveStatus(extra: Partial<SyncStatusView> = {}): SyncStatusView {
  const base = makeStatus().config!;
  return makeStatus({
    ready: false,
    last: null,
    concurrency: { protected: true, notes: ["服务器返回了 ETag"], checked_at: 1 },
    config: { ...base, provider: "onedrive", url: "", directory: "", username: "", account: ACCOUNT },
    ...extra,
  });
}

function notConfigured(extra: Partial<SyncStatusView> = {}): SyncStatusView {
  return makeStatus({ configured: false, config: null, ready: false, concurrency: null, ...extra });
}

beforeEach(() => {
  // Radix Select 在 jsdom 下要的几个浏览器 API（打开下拉框用）。
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
  Element.prototype.scrollIntoView = () => {};
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  for (const fn of Object.values(api)) fn.mockReset();
  api.syncConflictsList.mockResolvedValue([]);
  api.syncBackupsList.mockResolvedValue([]);
  useSync.setState({ status: null, probe: null, wantSettingsSection: false });
});

afterEach(cleanup);

function show(status: SyncStatusView) {
  api.syncStatus.mockResolvedValue(status);
  useSync.setState({ status, probe: null });
  return render(<SyncSection />);
}

/** 没连接存储时打开表单，并把存储类型切到 OneDrive。 */
async function openOnedriveForm(status = notConfigured()) {
  show(status);
  const trigger = screen.getByText("WebDAV（Nextcloud、坚果云、Alist、群晖……）");
  fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false, pointerType: "mouse" });
  fireEvent.click(await screen.findByRole("option", { name: "OneDrive（用微软账号登录）" }));
  await screen.findByTestId("onedrive-fields");
}

describe("OneDrive 存储选择和登录面板", () => {
  it("存储类型里有 OneDrive，选中后是登录面板，没有“保存并测试连接”", async () => {
    await openOnedriveForm();
    const text = screen.getByTestId("onedrive-fields").textContent ?? "";
    expect(text).toContain("用微软账号登录");
    expect(text).toContain("系统默认浏览器");
    expect(text).toContain("应用专用文件夹");
    expect(text).toContain("个人微软账号");
    expect(text).toContain("工作或学校账号暂不支持");
    expect((screen.getByRole("button", { name: "用微软账号登录" }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("button", { name: "保存并测试连接" })).toBeNull();
    expect(screen.queryByTestId("onedrive-no-client")).toBeNull();
  });

  it("没有内置 client ID 的版本：按钮不可用并说明原因，“高级”里填格式正确的才能用", async () => {
    await openOnedriveForm(notConfigured({ onedrive_builtin_client: false }));
    const button = screen.getByRole("button", { name: "用微软账号登录" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(screen.getByTestId("onedrive-no-client").textContent).toContain("没有内置");
    expect(screen.getByTestId("onedrive-no-client").textContent).toContain("高级");

    const input = screen.getByLabelText("client ID");
    fireEvent.change(input, { target: { value: "oops" } });
    expect(button.disabled).toBe(true);
    expect(screen.getByRole("alert").textContent).toContain("格式不对");
    fireEvent.change(input, { target: { value: CUSTOM_ID } });
    expect(button.disabled).toBe(false);
    expect(screen.queryByTestId("onedrive-no-client")).toBeNull();
  });

  it("填了自己的 client ID 就把它交给后端", async () => {
    api.syncOnedriveLogin.mockResolvedValue(onedriveStatus());
    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: onedriveStatus() });
    await openOnedriveForm(notConfigured({ onedrive_builtin_client: false }));
    fireEvent.change(screen.getByLabelText("client ID"), { target: { value: ` ${CUSTOM_ID} ` } });
    fireEvent.click(screen.getByRole("button", { name: "用微软账号登录" }));
    await waitFor(() => expect(api.syncOnedriveLogin).toHaveBeenCalledWith(CUSTOM_ID));
  });
});

describe("OneDrive 登录过程", () => {
  it("登录中显示“正在浏览器中登录…”、代理说明和取消；取消后按钮恢复并显示原因", async () => {
    let rejectLogin: (reason: unknown) => void = () => {};
    api.syncOnedriveLogin.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectLogin = reject;
      }),
    );
    api.syncOnedriveLoginCancel.mockResolvedValue(undefined);
    await openOnedriveForm();
    fireEvent.click(screen.getByRole("button", { name: "用微软账号登录" }));

    const progress = await screen.findByTestId("onedrive-logging-in");
    expect(progress.textContent).toContain("正在浏览器中登录…");
    expect(progress.textContent).toContain("系统浏览器自己的网络设置");
    expect(progress.textContent).toContain("软件网络");
    expect(screen.queryByRole("button", { name: "用微软账号登录" })).toBeNull();
    expect(api.syncOnedriveLogin).toHaveBeenCalledWith("");

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(api.syncOnedriveLoginCancel).toHaveBeenCalledTimes(1);
    await act(async () => {
      rejectLogin("已取消登录");
    });
    expect((await screen.findByTestId("sync-connection-error")).textContent).toBe("已取消登录");
    expect(screen.queryByTestId("onedrive-logging-in")).toBeNull();
    expect((screen.getByRole("button", { name: "用微软账号登录" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("用户在浏览器里拒绝授权、超时等原因如实显示", async () => {
    api.syncOnedriveLogin.mockRejectedValue("你在浏览器里取消了授权，没有登录");
    await openOnedriveForm();
    fireEvent.click(screen.getByRole("button", { name: "用微软账号登录" }));
    expect((await screen.findByTestId("sync-connection-error")).textContent).toBe(
      "你在浏览器里取消了授权，没有登录",
    );
    api.syncOnedriveLogin.mockRejectedValue("5 分钟内没有完成登录，已经停止等待，可以重新登录");
    fireEvent.click(screen.getByRole("button", { name: "用微软账号登录" }));
    await waitFor(() =>
      expect(screen.getByTestId("sync-connection-error").textContent).toContain("5 分钟"),
    );
  });

  it("登录成功：显示已连接的账号，并自动测试连接", async () => {
    const connected = onedriveStatus();
    api.syncOnedriveLogin.mockResolvedValue(connected);
    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: connected });
    await openOnedriveForm();
    fireEvent.click(screen.getByRole("button", { name: "用微软账号登录" }));

    const summary = await screen.findByTestId("onedrive-summary");
    expect(screen.getByTestId("onedrive-account").textContent).toContain(ACCOUNT);
    expect(summary.textContent).toContain("OneDrive（个人微软账号）");
    expect(summary.textContent).toContain("应用专用文件夹");
    expect(summary.textContent).toContain("不会进同步包");
    // 登录后自动测试连接（没带参数 = 测已保存的），结果显示在下面。
    await waitFor(() => expect(api.syncTestConnection).toHaveBeenCalledWith());
    expect((await screen.findByTestId("sync-probe")).textContent).toContain("已连接，云端还没有同步数据");
    // 摘要里没有令牌之类的东西，也没有“保存并测试连接”。
    expect(screen.queryByRole("button", { name: "保存并测试连接" })).toBeNull();
    // 已连接之后现有的主密码流程照常出现。
    expect(screen.getByRole("button", { name: "测试连接" })).toBeTruthy();
  });
});

describe("OneDrive 已连接", () => {
  it("登录失效时提示重新登录，重新登录按钮重新走登录", async () => {
    const expired = onedriveStatus({ auth_paused: "OneDrive 登录已失效，请重新登录" });
    api.syncOnedriveLogin.mockResolvedValue(onedriveStatus());
    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: onedriveStatus() });
    show(expired);
    expect(screen.getByTestId("onedrive-login-expired").textContent).toContain(
      "OneDrive 登录已失效，请重新登录",
    );
    fireEvent.click(screen.getByRole("button", { name: "重新登录" }));
    await waitFor(() => expect(api.syncOnedriveLogin).toHaveBeenCalledTimes(1));
    // 重新登录成功后，失效提示消失。
    await waitFor(() => expect(screen.queryByTestId("onedrive-login-expired")).toBeNull());
  });

  it("状态栏也说“OneDrive 登录已失效，请重新登录”", () => {
    useSync.setState({ status: onedriveStatus({ auth_paused: "x", ready: true, password_in_memory: true }) });
    render(<SyncStatusBar onOpen={() => {}} />);
    const bar = screen.getByTestId("sync-status-bar");
    expect(bar.textContent).toContain("OneDrive 登录已失效，请重新登录");
    expect(bar.getAttribute("data-tone")).toBe("error");
  });

  it("断开连接要先确认；断开后给出撤销授权的页面入口", async () => {
    api.syncOnedriveDisconnect.mockResolvedValue(notConfigured());
    api.syncOnedriveOpenRevokePage.mockResolvedValue(undefined);
    show(onedriveStatus());
    fireEvent.click(screen.getByRole("button", { name: "断开连接" }));
    expect(api.syncOnedriveDisconnect).not.toHaveBeenCalled();
    expect(screen.getByText(/云端已有的同步包不会被删除/)).toBeTruthy();
    // 先点“取消”：什么也没发生。
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(api.syncOnedriveDisconnect).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "断开连接" }));
    fireEvent.click(screen.getByRole("button", { name: "确定断开" }));
    await waitFor(() => expect(api.syncOnedriveDisconnect).toHaveBeenCalledTimes(1));

    // 回到 OneDrive 登录面板（不是跳回 WebDAV），并提示怎么彻底撤销授权。
    const fields = await screen.findByTestId("onedrive-fields");
    expect(fields.textContent).toContain("已断开连接");
    const hint = screen.getByTestId("onedrive-revoke-hint");
    expect(hint.textContent).toContain("应用和服务");
    fireEvent.click(screen.getByRole("button", { name: /打开“应用和服务”页面/ }));
    expect(api.syncOnedriveOpenRevokePage).toHaveBeenCalledTimes(1);
  });

  it("已连接的摘要里始终有撤销授权的说明", () => {
    show(onedriveStatus());
    expect(screen.getByTestId("onedrive-revoke-hint").textContent).toContain(
      "断开连接只删除这台电脑上保存的登录信息",
    );
  });

  it("自己填过 client ID 的连接会在摘要里说明", () => {
    const base = onedriveStatus().config!;
    show(onedriveStatus({ config: { ...base, client_id: CUSTOM_ID } }));
    expect(screen.getByTestId("onedrive-summary").textContent).toContain(CUSTOM_ID);
  });
});
