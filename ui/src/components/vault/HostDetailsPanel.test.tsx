/** @vitest-environment jsdom */
//! 主机详情表单（PERF-2）：新建主机的用户名默认 root；密码字段在「用户名」和「认证」之间。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Host } from "../../api/vault";

const mocks = vi.hoisted(() => ({
  saveHost: vi.fn(),
}));

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  hostMemoryGet: vi.fn(async () => null),
  hostMemorySet: vi.fn(async () => {}),
  saveProxy: vi.fn(),
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
  username: "deploy",
  auth_method: "password",
  identity_id: null,
  proxy_id: null,
  jump_host_id: null,
  proxy_mode: "default",
  tags: [],
  sort: 0,
  legacy_algorithms: false,
  created_at: 0,
  updated_at: 0,
};

afterEach(cleanup);

beforeEach(() => {
  mocks.saveHost.mockReset();
  mocks.saveHost.mockImplementation(async () => HOST);
  useVault.setState({ groups: [], hosts: [], identities: [], proxies: [], loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

function mount(host: Host | null, onError: (message: string) => void = () => {}) {
  const onSaved = vi.fn();
  const view = render(
    <HostDetailsPanel
      host={host}
      groupId={null}
      onClose={() => {}}
      onSaved={onSaved}
      onDelete={() => {}}
      onError={onError}
    />,
  );
  return { onSaved, view };
}

/** 表单里“某个标签”下面的输入框。 */
function inputOf(label: string): HTMLInputElement {
  const node = screen.getAllByText(label).find((el) => el.tagName === "LABEL");
  const input = node?.parentElement?.querySelector("input");
  if (!input) throw new Error(`找不到「${label}」的输入框`);
  return input as HTMLInputElement;
}

describe("新建主机的用户名默认值", () => {
  it("新建：用户名框默认是 root，端口默认 22，都能直接改", () => {
    mount(null);
    expect(inputOf("用户名").value).toBe("root");
    expect(inputOf("端口").value).toBe("22");
    fireEvent.change(inputOf("用户名"), { target: { value: "ubuntu" } });
    expect(inputOf("用户名").value).toBe("ubuntu");
  });

  it("新建后不改用户名直接保存：存的就是 root", async () => {
    mount(null);
    fireEvent.change(inputOf("名称"), { target: { value: "new-1" } });
    fireEvent.change(inputOf("地址"), { target: { value: "10.0.0.12" } });
    await act(async () => screen.getByText("保存").click());
    expect(mocks.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ id: null, username: "root" }),
    );
  });

  it("清空用户名再保存：仍按原规则拒绝", async () => {
    const onError = vi.fn();
    mount(null, onError);
    fireEvent.change(inputOf("名称"), { target: { value: "new-1" } });
    fireEvent.change(inputOf("地址"), { target: { value: "10.0.0.12" } });
    fireEvent.change(inputOf("用户名"), { target: { value: "" } });
    await act(async () => screen.getByText("保存").click());
    expect(onError).toHaveBeenCalledWith("名称、地址、用户名都不能为空");
    expect(mocks.saveHost).not.toHaveBeenCalled();
  });

  it("编辑已有主机：显示它保存的用户名，不是 root", () => {
    mount(HOST);
    expect(inputOf("用户名").value).toBe("deploy");
  });
});

describe("密码字段的位置", () => {
  /** 判断 a 在 b 前面（文档顺序）。 */
  const before = (a: Node, b: Node) =>
    Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);

  function checkOrder() {
    const labelOf = (text: string) =>
      screen.getAllByText(text).find((el) => el.tagName === "LABEL") as HTMLElement;
    const username = labelOf("用户名");
    const password = document.querySelector('input[type="password"]') as HTMLElement;
    // 密码框和小眼睛按钮包在一层 div 里：标签紧挨着这一层
    expect(password.parentElement?.previousElementSibling?.textContent).toContain("密码");
    const remember = screen.getByText("没有选凭据时，把密码存成一条新凭据");
    const authHeading = screen.getByText("认证", { selector: "h4" });
    const authMethod = labelOf("认证方式");
    expect(before(username, password)).toBe(true);
    expect(before(password, remember)).toBe(true);
    expect(before(remember, authHeading)).toBe(true);
    expect(before(authHeading, authMethod)).toBe(true);
  }

  it("新建：用户名 → 密码（连同开关）→ 认证", () => {
    mount(null);
    checkOrder();
  });

  it("编辑：顺序一样，提示文案不变", () => {
    mount(HOST);
    checkOrder();
    expect(screen.getByText("留空表示不保存密码，连接时会提示")).not.toBeNull();
  });
});

describe("新建主机的代理默认值（UI-6）", () => {
  async function fillAndSave() {
    fireEvent.change(inputOf("名称"), { target: { value: "new-1" } });
    fireEvent.change(inputOf("地址"), { target: { value: "10.0.0.12" } });
    await act(async () => screen.getByText("保存").click());
  }

  it("新建：代理下拉默认是「不用代理」，保存出去的 proxy_mode 是 none", async () => {
    mount(null);
    expect(screen.getByText("不用代理")).not.toBeNull();
    expect(screen.queryByText("跟随默认代理")).toBeNull();
    await fillAndSave();
    expect(mocks.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ id: null, proxy_mode: "none", proxy_id: null }),
    );
  });

  it("编辑已有主机：保持它保存的代理方式（跟随默认的仍是跟随默认）", async () => {
    mount({ ...HOST, proxy_mode: "default" });
    expect(screen.getByText("跟随默认代理")).not.toBeNull();
    await act(async () => screen.getByText("保存").click());
    expect(mocks.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ id: 3, proxy_mode: "default" }),
    );
  });

  it("编辑已有主机：指定代理的主机原样保存", async () => {
    mount({ ...HOST, proxy_mode: "custom", proxy_id: 5 });
    await act(async () => screen.getByText("保存").click());
    expect(mocks.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ id: 3, proxy_mode: "custom", proxy_id: 5 }),
    );
  });
});
