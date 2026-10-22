/** @vitest-environment jsdom */
//! 主机详情里的「新建 / 管理代理」（R-11）：
//! 以前「用哪个代理」下拉框只有"（请选择）"，代理列表为空时用户找不到在哪新建。
//! 现在从主机详情直接打开代理面板，新建完自动选中，回来时没保存的编辑还在。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Host, Proxy, ProxyDraft } from "../../api/vault";

const mocks = vi.hoisted(() => ({
  proxies: [] as Proxy[],
  saveProxy: vi.fn(),
  saveHost: vi.fn(),
}));

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => mocks.proxies),
  hostMemoryGet: vi.fn(async () => null),
  hostMemorySet: vi.fn(async () => {}),
  saveProxy: mocks.saveProxy,
  saveHost: mocks.saveHost,
  saveIdentity: vi.fn(),
  saveGroup: vi.fn(),
  deleteGroup: vi.fn(),
  deleteHost: vi.fn(),
  deleteIdentity: vi.fn(),
  deleteProxy: vi.fn(),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/auth", () => ({
  hostkeySaved: vi.fn(async () => null),
  hostkeyForget: vi.fn(async () => 0),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import HostDetailsPanel from "./HostDetailsPanel";
import { useVault } from "../../stores/vault";

const HOST: Host = {
  id: 3,
  group_id: null,
  name: "web-1",
  host: "10.0.0.11",
  port: 22,
  username: "root",
  auth_method: "agent",
  identity_id: null,
  proxy_id: null,
  jump_host_id: null,
  proxy_mode: "custom",
  tags: [],
  sort: 0,
  legacy_algorithms: false,
  created_at: 0,
  updated_at: 0,
};

// vitest 没开 globals，RTL 不会自动卸载上一个用例渲染的东西
afterEach(cleanup);

beforeEach(() => {
  mocks.proxies = [];
  mocks.saveProxy.mockReset();
  mocks.saveProxy.mockImplementation(async (draft: ProxyDraft) => {
    const saved: Proxy = {
      id: 42,
      name: draft.name,
      kind: draft.kind,
      host: draft.host,
      port: draft.port,
      username: draft.username,
      has_secret: false,
    };
    mocks.proxies = [saved];
    return saved;
  });
  mocks.saveHost.mockReset();
  mocks.saveHost.mockImplementation(async () => HOST);
  useVault.setState({ groups: [], hosts: [], identities: [], proxies: [], loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

function mount() {
  const onSaved = vi.fn();
  render(
    <HostDetailsPanel
      host={HOST}
      groupId={null}
      onClose={() => {}}
      onSaved={onSaved}
      onDelete={() => {}}
      onError={() => {}}
    />,
  );
  return { onSaved };
}

describe("主机详情的代理入口", () => {
  it("没有代理时：下拉框下面有提示，也有「新建 / 管理代理」", () => {
    mount();
    expect(screen.getByText(/还没有代理/)).not.toBeNull();
    expect(screen.getByText("新建 / 管理代理")).not.toBeNull();
  });

  it("新建代理后回到主机详情：没保存的编辑还在，新代理自动选中并随主机保存", async () => {
    const { onSaved } = mount();
    // 先改一个字段，模拟"正在编辑、还没保存"
    fireEvent.change(screen.getByDisplayValue("web-1"), { target: { value: "web-1-edited" } });

    await act(async () => screen.getByText("新建 / 管理代理").click());
    // 代理列表为空：直接是新建表单
    expect(screen.getAllByText("新建代理")).toHaveLength(2); // 页头按钮 + 表单标题
    const [nameInput] = screen.getAllByRole("textbox");
    fireEvent.change(nameInput, { target: { value: "proxy-a" } });
    fireEvent.change(screen.getByPlaceholderText("127.0.0.1"), { target: { value: "127.0.0.1" } });
    await act(async () => screen.getByText("保存").click());

    await waitFor(() => expect(screen.getByText("主机详情")).not.toBeNull());
    expect(screen.getByDisplayValue("web-1-edited")).not.toBeNull();
    expect(screen.getByText("proxy-a（socks5）")).not.toBeNull();

    await act(async () => screen.getByText("保存").click());
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(mocks.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ name: "web-1-edited", proxy_mode: "custom", proxy_id: 42 }),
    );
  });

  it("从代理面板点返回：不新建也回到主机详情，编辑还在", async () => {
    mount();
    fireEvent.change(screen.getByDisplayValue("web-1"), { target: { value: "web-2" } });
    await act(async () => screen.getByText("新建 / 管理代理").click());
    expect(screen.queryByText("主机详情")).toBeNull();
    await act(async () => screen.getByTitle("关闭").click());
    expect(screen.getByText("主机详情")).not.toBeNull();
    expect(screen.getByDisplayValue("web-2")).not.toBeNull();
  });
});
