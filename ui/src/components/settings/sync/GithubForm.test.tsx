/** @vitest-environment jsdom */
//! 同步页里 GitHub Gist 的部分（任务卡 S-3）：表单文案（如实说明 secret Gist）、令牌说明、
//! 新建 Gist、http 确认、已保存时的摘要、令牌失效和限流的显示。

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  read: { ok: true, message: "令牌有效，Gist 存在，里面还没有同步数据" },
  write: { ok: true, message: "令牌有 gist 权限（经典令牌）" },
  concurrency: {
    mode: "read_before_write",
    notes: ["GitHub Gist 的修改接口（PATCH）不支持 If-Match 这类条件写入"],
  },
  has_existing_data: false,
};

const GIST_ID = "mockgist00000000000000000000001";

function githubStatus(extra: Partial<SyncStatusView> = {}): SyncStatusView {
  return makeStatus({
    concurrency: {
      protected: false,
      notes: ["GitHub Gist 的修改接口不支持 If-Match 这类条件写入，只能写之前再读一次、比较版本号"],
      checked_at: 0,
    },
    config: {
      provider: "github",
      url: "",
      directory: "",
      username: "",
      allow_insecure_http: false,
      endpoint: "",
      region: "",
      bucket: "",
      access_key: "",
      prefix: "",
      path_style: true,
      api_base: "https://api.github.com",
      gist_id: GIST_ID,
      account: "",
      client_id: "",
      has_secret: true,
      plain_http: false,
    },
    ...extra,
  });
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

/** 没连接存储时打开表单，并把存储类型切到 GitHub Gist。 */
async function openGithubForm() {
  show(makeStatus({ configured: false, config: null, ready: false, concurrency: null }));
  const trigger = screen.getByText("WebDAV（Nextcloud、坚果云、Alist、群晖……）");
  fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false, pointerType: "mouse" });
  fireEvent.click(await screen.findByRole("option", { name: "GitHub Gist（个人访问令牌）" }));
  await screen.findByTestId("github-fields");
}

describe("GitHub Gist 表单", () => {
  it("如实说明 secret Gist 拿到链接就能读，不写“私有”，并附创建令牌的说明", async () => {
    await openGithubForm();
    const note = screen.getByTestId("github-secret-gist-note").textContent ?? "";
    expect(note).toContain("secret Gist");
    expect(note).toContain("任何拿到它链接的人都能打开");
    expect(note).toContain("密文");
    expect(note).toContain("zssh-sync.json");

    const help = screen.getByTestId("github-token-help").textContent ?? "";
    expect(help).toContain("经典令牌");
    expect(help).toContain("gist");
    expect(help).toContain("细粒度令牌");
    expect(help).toContain("Gists");
    expect(help).toContain("Read and write");
    expect(help).toContain("过期时间");
    expect(help).toContain("令牌已失效，请重新填写");

    // Gist 没有真正“私有”的：整个表单和页面里都不能出现这个说法。
    expect(screen.getByTestId("github-fields").textContent).not.toContain("私有");
    expect(document.body.textContent).not.toContain("私有 Gist");
  });

  it("没填令牌和 Gist ID 之前不能保存；填了才能点“保存并测试连接”", async () => {
    await openGithubForm();
    const save = screen.getByRole("button", { name: "保存并测试连接" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("GitHub 令牌"), { target: { value: "ghp_example" } });
    expect(save.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Gist ID"), { target: { value: "abc123" } });
    expect(save.disabled).toBe(false);

    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: githubStatus() });
    fireEvent.click(save);
    await waitFor(() => expect(api.syncTestConnection).toHaveBeenCalled());
    expect(api.syncTestConnection.mock.calls[0][0]).toMatchObject({
      provider: "github",
      token: "ghp_example",
      gist_id: "abc123",
      api_base: "",
      allow_insecure_http: false,
    });
  });

  it("新建 Gist：令牌填了才能点；成功后用新 ID 自动测试连接", async () => {
    await openGithubForm();
    const create = screen.getByRole("button", { name: "新建 Gist" }) as HTMLButtonElement;
    expect(create.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("GitHub 令牌"), { target: { value: "ghp_example" } });
    expect(create.disabled).toBe(false);

    api.syncGithubCreateGist.mockResolvedValue({ gist_id: GIST_ID, status: githubStatus() });
    api.syncTestConnection.mockResolvedValue({ probe: GOOD_PROBE, status: githubStatus() });
    fireEvent.click(create);
    await waitFor(() => expect(api.syncGithubCreateGist).toHaveBeenCalled());
    expect(api.syncGithubCreateGist.mock.calls[0][0]).toMatchObject({
      provider: "github",
      token: "ghp_example",
      gist_id: "",
    });
    await waitFor(() => expect(api.syncTestConnection).toHaveBeenCalled());
    expect(api.syncTestConnection.mock.calls[0][0]).toMatchObject({ gist_id: GIST_ID });
  });

  it("已填 Gist ID 时不能再新建（不会重复建出一堆 Gist）", async () => {
    await openGithubForm();
    fireEvent.change(screen.getByLabelText("GitHub 令牌"), { target: { value: "ghp_example" } });
    fireEvent.change(screen.getByLabelText("Gist ID"), { target: { value: "abc123" } });
    expect((screen.getByRole("button", { name: "新建 Gist" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it("新建失败：原因显示在卡片里，不弹窗，表单保持打开", async () => {
    await openGithubForm();
    fireEvent.change(screen.getByLabelText("GitHub 令牌"), { target: { value: "ghp_bad" } });
    api.syncGithubCreateGist.mockRejectedValue(
      "GitHub 令牌已失效，请重新填写令牌（可能已过期、被撤销或填错了）",
    );
    fireEvent.click(screen.getByRole("button", { name: "新建 Gist" }));
    await waitFor(() => expect(screen.getByTestId("sync-connection-error")).toBeTruthy());
    expect(screen.getByTestId("sync-connection-error").textContent).toContain("令牌已失效");
    expect(screen.getByTestId("sync-connection-form")).toBeTruthy();
  });

  it("接口地址是 http:// 时警告令牌明文发送，勾选确认才能保存", async () => {
    await openGithubForm();
    fireEvent.change(screen.getByLabelText("GitHub 令牌"), { target: { value: "ghp_x" } });
    fireEvent.change(screen.getByLabelText("Gist ID"), { target: { value: "abc" } });
    fireEvent.change(screen.getByLabelText("接口地址"), {
      target: { value: "http://ghe.example.internal/api/v3" },
    });
    expect(screen.getByRole("alert").textContent).toContain("令牌会以明文发送");
    const save = screen.getByRole("button", { name: "保存并测试连接" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText("我了解令牌会以明文发送"));
    expect(save.disabled).toBe(false);
  });
});

describe("GitHub Gist 已保存时", () => {
  it("摘要显示 Gist ID 和“第二台电脑填这个 ID”，不显示令牌；并发保护如实提示不支持", () => {
    show(githubStatus());
    const summary = screen.getByTestId("sync-connection-summary");
    expect(summary.textContent).toContain("GitHub Gist");
    expect(screen.getByTestId("github-summary-gist-id").textContent).toContain(GIST_ID);
    expect(summary.textContent).toContain("第二台电脑上填同一个 Gist ID");
    expect(summary.textContent).toContain("令牌已保存在本机（加密，不显示）");
    expect(summary.textContent).toContain("拿到链接的人都能打开");
    expect(summary.textContent).not.toContain("私有");

    const notice = screen.getByTestId("sync-concurrency-notice");
    expect(notice.textContent).toContain("不支持并发保护");
    expect(notice.textContent).toContain("很小的概率覆盖");
    // 令牌的内容界面上拿不到：状态里只有 has_secret。
    expect(document.body.textContent).not.toContain("ghp_");
  });

  it("修改：令牌框是空的并提示“已保存，留空沿用”，Gist ID 带出来，默认接口地址显示成空", () => {
    show(githubStatus());
    fireEvent.click(screen.getByRole("button", { name: "修改" }));
    const token = screen.getByLabelText("GitHub 令牌") as HTMLInputElement;
    expect(token.value).toBe("");
    expect(token.placeholder).toContain("已保存");
    expect((screen.getByLabelText("Gist ID") as HTMLInputElement).value).toBe(GIST_ID);
    expect((screen.getByLabelText("接口地址") as HTMLInputElement).value).toBe("");
    // 令牌留空也能保存测试（沿用已保存的）。
    expect(
      (screen.getByRole("button", { name: "保存并测试连接" }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("令牌失效：原因写在同步页和状态栏上", () => {
    const status = githubStatus({
      last: {
        at: 1_700_000_000,
        status: "error",
        summary: "GitHub 令牌已失效，请重新填写令牌（可能已过期、被撤销或填错了）",
        version: null,
        conflicts: 0,
        auto: true,
      },
    });
    show(status);
    expect(screen.getByTestId("sync-last-error").textContent).toContain("令牌已失效，请重新填写");
    cleanup();
    useSync.setState({ status });
    render(<SyncStatusBar onOpen={() => {}} />);
    expect(screen.getByTestId("sync-status-bar").textContent).toContain("令牌已失效");
  });

  it("限流：同步页和状态栏写“GitHub 限流，约 N 分钟后重试”", () => {
    const status = githubStatus({
      last: {
        at: 1_700_000_000,
        status: "error",
        summary: "GitHub 限流，约 5 分钟后重试",
        version: null,
        conflicts: 0,
        auto: false,
      },
    });
    show(status);
    expect(screen.getByTestId("sync-last-error").textContent).toContain(
      "GitHub 限流，约 5 分钟后重试",
    );
    cleanup();
    useSync.setState({ status });
    render(<SyncStatusBar onOpen={() => {}} />);
    const bar = screen.getByTestId("sync-status-bar");
    expect(bar.textContent).toContain("GitHub 限流，约 5 分钟后重试");
    expect(bar.getAttribute("data-tone")).toBe("error");
  });
});
