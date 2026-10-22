/** @vitest-environment jsdom */
//! 同步页的各个状态（任务卡 S-2 第 5.3 节）：未配置、连接失败、需要主密码、同步中、
//! 成功、失败、需要确认；主密码表单；冲突列表的恢复；备份列表的恢复；状态栏。

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ConflictView, BackupDetail, ConnectionProbe } from "../../../api/sync";
import { makeStatus, PENDING } from "../../../state/syncFixtures";
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
  concurrency: { mode: "conditional", notes: ["服务器返回了 ETag"] },
  has_existing_data: false,
};

const CONFLICT: ConflictView = {
  id: 7,
  at: 1_700_000_000,
  entity: "host",
  key: "uid-1",
  name: "web-1",
  kept_summary: "已删除",
  kept_device: null,
  kept_is_local: false,
  other_summary: "root@web-1.example.internal:22",
  other_device: "dev-local",
  other_is_local: true,
  deletion: true,
  can_use_other: true,
  message: "",
};

const BACKUP: BackupDetail = {
  path: "C:\\data\\backups\\zssh-backup-20260101-000000.bin",
  name: "zssh-backup-20260101-000000.bin",
  size: 1000,
  stamp: "20260101-000000",
  sequence: 1,
  readable: true,
  created_at: 1_767_225_600,
  app_version: "0.1.0",
  record_count: 12,
  host_count: 9,
};

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
  api.syncStatus.mockResolvedValue(makeStatus());
  api.syncConflictsList.mockResolvedValue([]);
  api.syncBackupsList.mockResolvedValue([]);
  useSync.setState({ status: null, probe: null, wantSettingsSection: false });
});

afterEach(cleanup);

function show(status = makeStatus(), probe: ConnectionProbe | null = null) {
  api.syncStatus.mockResolvedValue(status);
  useSync.setState({ status, probe });
  return render(<SyncSection />);
}

describe("未配置 / 连接", () => {
  it("没连接存储时只有连接表单，保存按钮在填完之前不可点", () => {
    show(makeStatus({ configured: false, config: null, ready: false }));
    expect(screen.getByTestId("sync-connection-form")).toBeTruthy();
    expect(screen.queryByTestId("sync-now")).toBeNull();
    const save = screen.getByRole("button", { name: "保存并测试连接" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("WebDAV 地址"), {
      target: { value: "https://dav.example.internal/" },
    });
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "alice" } });
    expect(save.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "secret" } });
    expect(save.disabled).toBe(false);
  });

  it("http:// 地址要警告密码明文发送，勾选确认才允许保存", () => {
    show(makeStatus({ configured: false, config: null, ready: false }));
    fireEvent.change(screen.getByLabelText("WebDAV 地址"), {
      target: { value: "http://192.168.1.5/dav" },
    });
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "alice" } });
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "secret" } });
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("密码会以明文发送");
    const save = screen.getByRole("button", { name: "保存并测试连接" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText("我了解密码会以明文发送"));
    expect(save.disabled).toBe(false);
  });

  it("保存并测试：读 / 写 / 并发保护分别显示，提交的是表单里的内容", async () => {
    show(makeStatus({ configured: false, config: null, ready: false }));
    api.syncTestConnection.mockResolvedValue({
      probe: GOOD_PROBE,
      status: makeStatus({ ready: false }),
    });
    fireEvent.change(screen.getByLabelText("WebDAV 地址"), {
      target: { value: "https://dav.example.internal/" },
    });
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "alice" } });
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并测试连接" }));
    await waitFor(() => expect(screen.getByTestId("sync-probe")).toBeTruthy());
    const input = api.syncTestConnection.mock.calls[0][0];
    expect(input).toMatchObject({
      provider: "webdav",
      url: "https://dav.example.internal/",
      username: "alice",
      password: "secret",
    });
    const probe = screen.getByTestId("sync-probe").textContent ?? "";
    expect(probe).toContain("连接与读取");
    expect(probe).toContain("写入权限");
    expect(probe).toContain("并发保护：支持");
  });

  it("连接失败：原因显示在卡片里，不弹窗", async () => {
    show(makeStatus({ configured: false, config: null, ready: false }));
    api.syncTestConnection.mockResolvedValue({
      probe: {
        read: { ok: false, message: "WebDAV 读取失败：用户名或密码不对" },
        write: { ok: false, message: "读取失败，没有继续检查写权限" },
        concurrency: null,
        has_existing_data: false,
      },
      status: makeStatus({ configured: true, ready: false }),
    });
    fireEvent.change(screen.getByLabelText("WebDAV 地址"), {
      target: { value: "https://dav.example.internal/" },
    });
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "alice" } });
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "bad" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并测试连接" }));
    await waitFor(() => expect(screen.getByTestId("sync-probe")).toBeTruthy());
    expect(screen.getByTestId("sync-probe").textContent).toContain("用户名或密码不对");
    // 失败后表单保持打开，用户可以改了再试。
    expect(screen.getByTestId("sync-connection-form")).toBeTruthy();
  });

  it("S3 表单：路径风格 / 虚拟主机风格可选，Secret 留空沿用已保存的", async () => {
    show(
      makeStatus({
        config: {
          provider: "s3",
          url: "",
          directory: "",
          username: "",
          allow_insecure_http: false,
          endpoint: "https://s3.example.internal",
          region: "us-east-1",
          bucket: "my-bucket",
          access_key: "AK",
          prefix: "z-ssh",
          path_style: false,
          api_base: "",
          gist_id: "",
          account: "",
          client_id: "",
          has_secret: true,
          plain_http: false,
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    expect((screen.getByLabelText("Endpoint") as HTMLInputElement).value).toBe(
      "https://s3.example.internal",
    );
    expect(screen.getByText("留空 = 继续用已保存的 Secret")).toBeTruthy();
    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: makeStatus() });
    fireEvent.click(screen.getByRole("button", { name: "保存并测试连接" }));
    await waitFor(() => expect(api.syncTestConnection).toHaveBeenCalled());
    expect(api.syncTestConnection.mock.calls[0][0]).toMatchObject({
      provider: "s3",
      bucket: "my-bucket",
      secret_key: "",
      path_style: false,
    });
  });

  it("不支持并发保护 / 还没检测：在页面上提示", () => {
    show(makeStatus({ concurrency: { protected: false, notes: ["没有返回 ETag"], checked_at: 1 } }));
    expect(screen.getByTestId("sync-concurrency-notice").textContent).toContain(
      "不支持并发保护",
    );
    cleanup();
    show(makeStatus({ concurrency: null }));
    expect(screen.getByTestId("sync-concurrency-notice").textContent).toContain("还没有检测");
  });
});

describe("主密码", () => {
  it("第一次设置：强调忘了无法恢复，要输入两次并勾选“我知道了”", async () => {
    show(makeStatus({ ready: false, password_in_memory: false, last: null }), GOOD_PROBE);
    const form = screen.getByTestId("sync-password-form");
    expect(form.textContent).toContain("忘记主密码就无法恢复云端数据");
    const submit = screen.getByRole("button", { name: "设置主密码并同步" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("主密码"), { target: { value: "Correct-Horse-9" } });
    expect(screen.getByTestId("password-strength").textContent).toContain("强");
    fireEvent.change(screen.getByLabelText("确认主密码"), { target: { value: "Correct-Horse-9" } });
    expect(submit.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText("我知道忘记主密码就无法恢复云端数据"));
    expect(submit.disabled).toBe(false);
    api.syncSetPassword.mockResolvedValue({
      status: "completed",
      summary: "ok",
      conflicts: 0,
      nulled_references: [],
      pending: null,
      status_view: makeStatus(),
    });
    fireEvent.click(screen.getByLabelText("在这台电脑上记住主密码"));
    fireEvent.click(submit);
    await waitFor(() => expect(api.syncSetPassword).toHaveBeenCalled());
    expect(api.syncSetPassword).toHaveBeenCalledWith(
      "Correct-Horse-9",
      true,
      "Correct-Horse-9",
    );
  });

  it("云端已有数据（第二台电脑）：只输入主密码；密码错误给明确提示", async () => {
    show(
      makeStatus({ ready: false, password_in_memory: false, last: null }),
      { ...GOOD_PROBE, has_existing_data: true },
    );
    expect(screen.getByTestId("sync-password-form").textContent).toContain("云端已经有同步数据");
    expect(screen.queryByLabelText("确认主密码")).toBeNull();
    api.syncSetPassword.mockRejectedValue("主密码错误（或文件头被改过）");
    fireEvent.change(screen.getByLabelText("主密码"), { target: { value: "wrong-password" } });
    fireEvent.click(screen.getByRole("button", { name: "解开并同步" }));
    await waitFor(() => expect(screen.getByTestId("sync-password-error")).toBeTruthy());
    expect(screen.getByTestId("sync-password-error").textContent).toContain("主密码错误");
    expect(api.syncSetPassword).toHaveBeenCalledWith("wrong-password", false, undefined);
  });

  it("没测试过连接就不知道云端有没有数据：先提示去测试，不给输入框", () => {
    show(makeStatus({ ready: false, password_in_memory: false, last: null }), null);
    expect(screen.getByTestId("sync-password-needs-test")).toBeTruthy();
    expect(screen.queryByLabelText("主密码")).toBeNull();
  });

  it("需要主密码（已设置过，但这次启动还没输入）", () => {
    show(makeStatus({ password_in_memory: false }), null);
    expect(screen.getByTestId("sync-password-form").textContent).toContain("需要主密码");
    expect((screen.getByTestId("sync-now") as HTMLButtonElement).disabled).toBe(true);
  });

  it("记住开关和修改主密码", async () => {
    show(makeStatus());
    api.syncRememberPassword.mockResolvedValue(makeStatus({ password_remembered: true }));
    fireEvent.click(screen.getByRole("switch", { name: "在这台电脑上记住主密码" }));
    await waitFor(() => expect(api.syncRememberPassword).toHaveBeenCalledWith(true));

    fireEvent.click(screen.getByRole("button", { name: "修改…" }));
    const dialog = await screen.findByTestId("change-password-dialog");
    const submit = within(dialog).getByRole("button", { name: "修改并上传" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.change(within(dialog).getByLabelText("旧主密码"), { target: { value: "old-password" } });
    fireEvent.change(within(dialog).getByLabelText("新主密码"), { target: { value: "new-password-1" } });
    fireEvent.change(within(dialog).getByLabelText("确认新主密码"), {
      target: { value: "new-password-2" },
    });
    expect(submit.disabled).toBe(true);
    expect(dialog.textContent).toContain("两次输入的新主密码不一致");
    fireEvent.change(within(dialog).getByLabelText("确认新主密码"), {
      target: { value: "new-password-1" },
    });
    expect(submit.disabled).toBe(false);
    api.syncChangePassword.mockResolvedValue(makeStatus());
    fireEvent.click(submit);
    await waitFor(() =>
      expect(api.syncChangePassword).toHaveBeenCalledWith("old-password", "new-password-1"),
    );
  });
});

describe("同步", () => {
  it("成功：显示上次同步的时间、结果和上传的版本", () => {
    show(makeStatus());
    expect(screen.getByText(/同步成功，上传版本/)).toBeTruthy();
    expect((screen.getByTestId("sync-now") as HTMLButtonElement).disabled).toBe(false);
  });

  it("同步中：按钮显示“同步中…”并禁用", () => {
    show(makeStatus({ running: true }));
    const button = screen.getByTestId("sync-now") as HTMLButtonElement;
    expect(button.textContent).toContain("同步中…");
    expect(button.disabled).toBe(true);
  });

  it("失败：原因显示在同步页，没有弹窗", () => {
    show(
      makeStatus({
        last: {
          at: 1_700_000_000,
          status: "error",
          summary: "无法连接到存储服务（网络不通或代理不可用）",
          version: null,
          conflicts: 0,
          auto: true,
        },
      }),
    );
    expect(screen.getByTestId("sync-last-error").textContent).toContain("无法连接到存储服务");
    expect(screen.queryByTestId("deletion-dialog")).toBeNull();
  });

  it("立即同步：调用后端，手动同步不带选择", async () => {
    show(makeStatus());
    api.syncNow.mockResolvedValue({
      status: "completed",
      summary: "ok",
      conflicts: 0,
      nulled_references: [],
      pending: null,
      status_view: makeStatus(),
    });
    fireEvent.click(screen.getByTestId("sync-now"));
    await waitFor(() => expect(api.syncNow).toHaveBeenCalledWith(undefined));
  });

  it("需要确认：弹对话框写清数量和名字，三个选择都能走通", async () => {
    show(makeStatus());
    api.syncNow.mockResolvedValueOnce({
      status: "needs_confirmation",
      summary: PENDING.summary,
      conflicts: 0,
      nulled_references: [],
      pending: PENDING,
      status_view: makeStatus({ pending: PENDING }),
    });
    fireEvent.click(screen.getByTestId("sync-now"));
    const dialog = await screen.findByTestId("deletion-dialog");
    expect(dialog.textContent).toContain("删除 12 条记录");
    expect(dialog.textContent).toContain("主机：web-1");
    expect(dialog.textContent).toContain("分组：生产");
    expect(dialog.textContent).toContain("还有 10 条");

    api.syncNow.mockResolvedValue({
      status: "completed",
      summary: "ok",
      conflicts: 0,
      nulled_references: [],
      pending: null,
      status_view: makeStatus(),
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "用云端版本覆盖本地" }));
    await waitFor(() => expect(api.syncNow).toHaveBeenLastCalledWith("use_cloud"));
  });

  it("自动同步停在确认上：不自己弹对话框，页面上有“处理”入口", async () => {
    show(makeStatus({ pending: { ...PENDING, auto: true } }));
    expect(screen.queryByTestId("deletion-dialog")).toBeNull();
    const banner = screen.getByTestId("sync-pending");
    expect(banner.textContent).toContain("自动同步已暂停");
    fireEvent.click(within(banner).getByRole("button", { name: "处理" }));
    const dialog = await screen.findByTestId("deletion-dialog");
    api.syncNow.mockResolvedValue({
      status: "cancelled",
      summary: "已取消",
      conflicts: 0,
      nulled_references: [],
      pending: null,
      status_view: makeStatus(),
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "继续" }));
    await waitFor(() => expect(api.syncNow).toHaveBeenCalledWith("continue"));
  });

  it("自动同步开关和间隔", async () => {
    show(makeStatus());
    api.syncAutoSet.mockResolvedValue(makeStatus({ auto: { enabled: false, interval_minutes: 15 } }));
    fireEvent.click(screen.getByRole("switch", { name: "自动同步" }));
    await waitFor(() => expect(api.syncAutoSet).toHaveBeenCalledWith(false, 15));
  });

  it("基线损坏：提供“重建同步基线”并说明后果", async () => {
    show(makeStatus({ baseline_broken: true }));
    const alert = screen.getByTestId("sync-baseline-broken");
    expect(alert.textContent).toContain("两边的数据都会保留");
    expect((screen.getByTestId("sync-now") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(alert).getByRole("button", { name: "重建同步基线…" }));
    api.syncResetBaseline.mockResolvedValue(makeStatus());
    fireEvent.click(await screen.findByRole("button", { name: "重建" }));
    await waitFor(() => expect(api.syncResetBaseline).toHaveBeenCalled());
  });
});

describe("冲突列表", () => {
  it("每条冲突写明类型、名称、保留的和另一份；被删的记录可以恢复", async () => {
    api.syncConflictsList.mockResolvedValue([CONFLICT]);
    show(makeStatus({ conflict_count: 1 }));
    const row = await screen.findByTestId("conflict-row");
    expect(row.textContent).toContain("主机");
    expect(row.textContent).toContain("web-1");
    expect(row.textContent).toContain("保留：删除");
    expect(row.textContent).toContain("本机 最后的内容：root@web-1.example.internal:22");
    api.syncConflictUseOther.mockResolvedValue(makeStatus());
    api.syncConflictsList.mockResolvedValue([]);
    fireEvent.click(within(row).getByRole("button", { name: "恢复这条记录" }));
    await waitFor(() => expect(api.syncConflictUseOther).toHaveBeenCalledWith(7));
    await waitFor(() => expect(screen.getByTestId("conflicts-empty")).toBeTruthy());
  });

  it("两边都改过的冲突叫“改用另一份”，“我知道了”清掉一条", async () => {
    api.syncConflictsList.mockResolvedValue([
      {
        ...CONFLICT,
        deletion: false,
        kept_summary: "root@a:22",
        kept_device: "dev-other-xyz",
        other_summary: "root@b:22",
      },
    ]);
    show(makeStatus({ conflict_count: 1 }));
    const row = await screen.findByTestId("conflict-row");
    expect(within(row).getByRole("button", { name: "改用另一份" })).toBeTruthy();
    expect(row.textContent).toContain("另一台电脑（dev-othe）");
    api.syncConflictDismiss.mockResolvedValue(makeStatus());
    fireEvent.click(within(row).getByRole("button", { name: "我知道了" }));
    await waitFor(() => expect(api.syncConflictDismiss).toHaveBeenCalledWith(7));
  });

  it("主机指纹冲突不给“改用另一份”，写明原因，只能“我知道了”", async () => {
    api.syncConflictsList.mockResolvedValue([
      {
        ...CONFLICT,
        entity: "known_host",
        name: "h.example.internal:22",
        deletion: false,
        kept_summary: "ssh-ed25519 公钥",
        other_summary: "ssh-ed25519 公钥",
        can_use_other: false,
        message: "主机指纹不能在这里改用另一份：替换已信任的公钥关系到连接安全。",
      },
    ]);
    show(makeStatus({ conflict_count: 1 }));
    const row = await screen.findByTestId("conflict-row");
    expect(within(row).queryByRole("button", { name: "改用另一份" })).toBeNull();
    expect(within(row).getByTestId("conflict-note").textContent).toContain("主机指纹不能在这里改用另一份");
    expect(within(row).getByRole("button", { name: "我知道了" })).toBeTruthy();
  });
});

describe("本地备份", () => {
  it("列出时间、应用版本、记录数；恢复前确认", async () => {
    api.syncBackupsList.mockResolvedValue([BACKUP]);
    show(makeStatus());
    const row = await screen.findByTestId("backup-row");
    expect(row.textContent).toContain("应用版本 0.1.0");
    expect(row.textContent).toContain("12 条记录（主机 9 台）");
    api.syncBackupRestore.mockResolvedValue({
      summary: "已恢复。恢复前的状态已自动备份为 zssh-backup-x.bin",
      safety_backup: "zssh-backup-x.bin",
      status: makeStatus(),
    });
    fireEvent.click(within(row).getByRole("button", { name: "恢复" }));
    // 点了“恢复”只弹确认，还没有恢复。
    expect(api.syncBackupRestore).not.toHaveBeenCalled();
    const confirm = await screen.findByRole("button", { name: "恢复", hidden: false });
    expect(confirm).toBeTruthy();
    const buttons = screen.getAllByRole("button", { name: "恢复" });
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() => expect(api.syncBackupRestore).toHaveBeenCalledWith(BACKUP.path));
    await waitFor(() => expect(screen.getByTestId("backup-restored")).toBeTruthy());
  });

  it("保留份数：显示当前值（默认 20），改了调后端", async () => {
    api.syncBackupsList.mockResolvedValue([BACKUP]);
    show(makeStatus());
    const trigger = (await screen.findByLabelText("备份保留份数")) as HTMLElement;
    expect(trigger.textContent).toContain("20 份");
    api.syncBackupKeepSet.mockResolvedValue(makeStatus({ backup_keep: 5 }));
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false, pointerType: "mouse" });
    fireEvent.click(await screen.findByRole("option", { name: "5 份" }));
    await waitFor(() => expect(api.syncBackupKeepSet).toHaveBeenCalledWith(5));
  });

  it("保留份数不在预设里（比如手动改过）也能显示出来", async () => {
    api.syncBackupsList.mockResolvedValue([]);
    show(makeStatus({ backup_keep: 33 }));
    const trigger = (await screen.findByLabelText("备份保留份数")) as HTMLElement;
    expect(trigger.textContent).toContain("33 份");
  });

  it("读不了的备份不能点恢复", async () => {
    api.syncBackupsList.mockResolvedValue([{ ...BACKUP, readable: false }]);
    show(makeStatus());
    const row = await screen.findByTestId("backup-row");
    expect((within(row).getByRole("button", { name: "恢复" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });
});

describe("状态栏", () => {
  it("失败原因显示在状态栏上，点一下去同步页", () => {
    const onOpen = vi.fn();
    useSync.setState({
      status: makeStatus({
        last: {
          at: 1,
          status: "error",
          summary: "主密码错误",
          version: null,
          conflicts: 0,
          auto: true,
        },
      }),
    });
    render(<SyncStatusBar onOpen={onOpen} />);
    const bar = screen.getByTestId("sync-status-bar");
    expect(bar.textContent).toContain("同步失败：主密码错误");
    expect(bar.getAttribute("data-tone")).toBe("error");
    fireEvent.click(bar);
    expect(onOpen).toHaveBeenCalled();
    expect(useSync.getState().wantSettingsSection).toBe(true);
  });

  it("没连接存储时不显示", () => {
    useSync.setState({ status: makeStatus({ configured: false, config: null }) });
    const { container } = render(<SyncStatusBar onOpen={() => {}} />);
    expect(container.innerHTML).toBe("");
  });
});
