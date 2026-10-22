/** @vitest-environment jsdom */
//! 设置页外壳（R-11）：右上角有关闭按钮；「连接」页的默认代理旁有新建入口，
//! 代理列表为空时给一句提示。AI 页等内容不在这里测。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "", known_hosts: "" })),
  openDataDir: vi.fn(async () => {}),
  sessionLogDirInfo: vi.fn(async () => ""),
  openSessionLogDir: vi.fn(async () => {}),
}));

import SettingsPage from "./SettingsPage";
import { useVault } from "../../stores/vault";

// vitest 没开 globals，RTL 不会自动卸载上一个用例渲染的东西
afterEach(cleanup);

beforeEach(() => {
  useVault.setState({ proxies: [], loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("设置页外壳", () => {
  it("右上角 × 调 onClose", async () => {
    const onClose = vi.fn();
    render(<SettingsPage onClose={onClose} />);
    await act(async () => screen.getByLabelText("关闭设置").click());
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("「连接」页：没有代理时有提示，「新建 / 管理代理」打开代理面板", async () => {
    const onManageProxies = vi.fn();
    render(<SettingsPage onClose={() => {}} onManageProxies={onManageProxies} />);
    await act(async () => screen.getByText("连接").click());
    expect(screen.getByText(/还没有代理/)).not.toBeNull();
    await act(async () => screen.getByText("新建 / 管理代理").click());
    expect(onManageProxies).toHaveBeenCalledTimes(1);
  });
});
