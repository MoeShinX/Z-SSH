/** @vitest-environment jsdom */
//! 代理面板的「新建」（R-11）：以前要靠 `name !== ""` 才显示表单，点「新建」清空名字后
//! 表单反而藏起来，用户没地方填。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  saveProxy: vi.fn(),
  deleteProxy: vi.fn(),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import ProxyPanel from "./ProxyPanel";
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

// 页头按钮也叫"新建代理"：只有它时页面上是 1 处，表单打开后加上表单标题是 2 处
const newProxyTexts = () => screen.getAllByText("新建代理").length;

describe("代理面板", () => {
  it("列表为空时有提示；点「新建代理」出现表单，点「取消」收起", async () => {
    render(<ProxyPanel onClose={() => {}} onError={() => {}} />);
    expect(screen.getAllByText(/还没有代理/).length).toBeGreaterThan(0);
    expect(newProxyTexts()).toBe(1);

    await act(async () => screen.getByRole("button", { name: "新建代理" }).click());
    expect(newProxyTexts()).toBe(2);

    await act(async () => screen.getByText("取消").click());
    expect(newProxyTexts()).toBe(1);
  });

  it("startCreating：一打开就是新建表单", () => {
    render(<ProxyPanel startCreating onClose={() => {}} onError={() => {}} />);
    expect(newProxyTexts()).toBe(2);
  });

  it("每一行都能直接删除，删除前要确认", async () => {
    const api = await import("../../api/vault");
    const proxy = {
      id: 7,
      name: "公司代理",
      kind: "socks5" as const,
      host: "127.0.0.1",
      port: 1080,
      username: null,
      has_secret: false,
    };
    useVault.setState({ proxies: [proxy], loaded: true });
    vi.mocked(api.deleteProxy).mockResolvedValue(undefined);
    render(<ProxyPanel onClose={() => {}} onError={() => {}} />);

    await act(async () => screen.getByTitle("删除").click());
    expect(api.deleteProxy).not.toHaveBeenCalled();
    expect(screen.getByText("删除代理「公司代理」？")).not.toBeNull();

    await act(async () => screen.getByRole("button", { name: "删除" }).click());
    expect(api.deleteProxy).toHaveBeenCalledWith(7);
  });
});
