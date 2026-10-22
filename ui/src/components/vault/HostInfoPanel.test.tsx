/** @vitest-environment jsdom */
//! 终端界面主机树的属性面板（UI-6 第 4 节）：单击展开、再单击收起、点另一台切换、
//! 双击不让面板来回闪；主机页（不传 infoPanel）单击行为不变。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ isMaximized: vi.fn(async () => false) }),
}));
vi.mock("../../api/vault", async (importOriginal) => ({
  draftOfHost: (await importOriginal<typeof import("../../api/vault")>()).draftOfHost,
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import type { Host, Proxy } from "../../api/vault";
import { setDoubleClickMsForTests } from "../../state/doubleClickTime";
import {
  COLLAPSE_MARGIN_MS,
  collapseDelayMs,
  resetHostInfoForTests,
  useHostInfoId,
} from "../../state/hostInfoPanel";
import { UNGROUPED_KEY } from "../../state/hostTree";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import { hostInfoRows } from "./HostInfoPanel";
import { HostTreeAside } from "./VaultView";

function host(id: number, over: Partial<Host> = {}): Host {
  return {
    id,
    group_id: null,
    name: `web-${id}`,
    host: `10.0.0.${id}`,
    port: 22,
    username: "ops",
    auth_method: "password",
    identity_id: null,
    proxy_id: null,
    jump_host_id: null,
    proxy_mode: "none",
    tags: [],
    sort: 0,
    legacy_algorithms: false,
    created_at: 0,
    updated_at: 0,
    ...over,
  } as Host;
}

const proxy: Proxy = {
  id: 7,
  name: "office-socks",
  kind: "socks5",
  host: "10.0.0.50",
  port: 1080,
  username: null,
  has_secret: false,
};

function setup(infoPanel: boolean) {
  const actions = {
    select: vi.fn(),
    connect: vi.fn(),
    openSftp: vi.fn(),
    remove: vi.fn(),
    copy: vi.fn(),
  };
  const noop = () => {};
  render(
    <HostTreeAside
      selectedGroup="all"
      onSelectGroup={noop}
      selectedHostId={null}
      hostActions={actions}
      onNewHost={noop}
      onNewGroup={noop}
      onEditGroup={noop}
      onDeleteGroup={noop}
      collapsedStyle="hidden"
      infoPanel={infoPanel}
    />,
  );
  return actions;
}

/** 记下面板展开的主机 id 的每一次变化（含收起成 null），用来断言"中途没收起过"。 */
function HostInfoProbe({ seen }: { seen: (number | null)[] }) {
  seen.push(useHostInfoId());
  return null;
}

const panel = () => document.querySelector<HTMLElement>('[data-section="host-info"]');
const leaf = (name: string) =>
  Array.from(document.querySelectorAll<HTMLElement>('[data-section="tree-host"]')).find((el) =>
    el.textContent?.includes(name),
  ) as HTMLElement;
const advance = (ms: number) => act(() => void vi.advanceTimersByTime(ms));

beforeEach(() => {
  vi.useFakeTimers();
  resetHostInfoForTests();
  setDoubleClickMsForTests(null);
  useVault.setState({
    groups: [],
    hosts: [
      host(1),
      host(2, { name: "db-2", username: "root", port: 2222, host: "2001:db8::2" }),
      host(3, { name: "app-3", jump_host_id: 1, proxy_mode: "custom", proxy_id: 7 }),
    ],
    proxies: [proxy],
    load: async () => {},
  } as never);
  useSettings.setState({
    "layout.vault_expanded": UNGROUPED_KEY,
    "layout.vault_sidebar_collapsed": false,
    "layout.host_tree_width": 224,
  });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("终端界面主机树的属性面板", () => {
  it("单击展开：名称、主机、用户名、协议、端口；再单击同一台收起", () => {
    const actions = setup(true);
    expect(panel()).toBeNull();

    fireEvent.click(leaf("web-1"));
    expect(panel()).not.toBeNull();
    expect(panel()?.textContent).toContain("web-1");
    expect(panel()?.textContent).toContain("10.0.0.1");
    expect(panel()?.textContent).toContain("ops");
    expect(panel()?.textContent).toContain("SSH");
    expect(panel()?.textContent).toContain("22");
    // 调用方自己的单击（高亮）照常执行
    expect(actions.select).toHaveBeenCalledTimes(1);

    fireEvent.click(leaf("web-1"));
    advance(collapseDelayMs() + 10);
    expect(panel()).toBeNull();
  });

  it("点另一台：面板内容切换，不收起", () => {
    setup(true);
    fireEvent.click(leaf("web-1"));
    fireEvent.click(leaf("db-2"));
    advance(collapseDelayMs() + 10);

    expect(panel()).not.toBeNull();
    expect(panel()?.textContent).toContain("db-2");
    expect(panel()?.textContent).toContain("root");
    expect(panel()?.textContent).toContain("2222");
    expect(panel()?.textContent).not.toContain("web-1");
  });

  it("双击（先后两次单击再 dblclick）：照常连接，面板不来回闪", () => {
    const actions = setup(true);
    const el = leaf("web-1");

    // 面板没开：双击 = 展开并保持
    fireEvent.click(el);
    fireEvent.click(el);
    fireEvent.doubleClick(el);
    advance(collapseDelayMs() + 10);
    expect(actions.connect).toHaveBeenCalledTimes(1);
    expect(panel()).not.toBeNull();

    // 面板已经开着：双击也不会在中途收起（收起被延迟，dblclick 到来时取消）
    fireEvent.click(el);
    expect(panel()).not.toBeNull();
    fireEvent.click(el);
    fireEvent.doubleClick(el);
    advance(collapseDelayMs() + 10);
    expect(actions.connect).toHaveBeenCalledTimes(2);
    expect(panel()).not.toBeNull();
  });

  it.each([350, 450])("较慢的双击（两下相隔 %dms）：面板全程没有收起过，连接只执行一次", (gap) => {
    const seen: (number | null)[] = [];
    render(<HostInfoProbe seen={seen} />);
    const actions = setup(true);
    const el = leaf("web-1");

    // 先展开（单击），再对开着的这台做慢双击
    fireEvent.click(el, { detail: 1 });
    expect(panel()).not.toBeNull();
    seen.length = 0;

    fireEvent.click(el, { detail: 1 });
    advance(gap);
    fireEvent.click(el, { detail: 2 });
    fireEvent.doubleClick(el, { detail: 2 });
    advance(collapseDelayMs() + 10);

    expect(seen.every((id) => id === 1)).toBe(true);
    expect(panel()).not.toBeNull();
    expect(actions.connect).toHaveBeenCalledTimes(1);
  });

  it("点击计数 >= 2 的单击不当作再点一下收起（即使收起已经过了延迟）", () => {
    setup(true);
    const el = leaf("web-1");
    fireEvent.click(el, { detail: 1 });
    fireEvent.click(el, { detail: 2 });
    advance(collapseDelayMs() + 10);
    expect(panel()).not.toBeNull();
    fireEvent.click(el, { detail: 3 });
    advance(collapseDelayMs() + 10);
    expect(panel()).not.toBeNull();
  });

  it("真正的单击（计数为 1）再点同一台仍会收起", () => {
    setup(true);
    const el = leaf("web-1");
    fireEvent.click(el, { detail: 1 });
    expect(panel()).not.toBeNull();
    fireEvent.click(el, { detail: 1 });
    advance(collapseDelayMs() + 10);
    expect(panel()).toBeNull();
  });

  it("收起延迟 = 系统双击时间 + 余量：默认 500ms 档", () => {
    setup(true);
    const el = leaf("web-1");
    fireEvent.click(el, { detail: 1 });
    fireEvent.click(el, { detail: 1 });
    advance(500 + COLLAPSE_MARGIN_MS - 10);
    expect(panel()).not.toBeNull();
    advance(20);
    expect(panel()).toBeNull();
  });

  it("系统双击时间调到 650ms：收起延迟跟着变长", () => {
    setDoubleClickMsForTests(650);
    expect(collapseDelayMs()).toBe(650 + COLLAPSE_MARGIN_MS);
    setup(true);
    const el = leaf("web-1");
    fireEvent.click(el, { detail: 1 });
    fireEvent.click(el, { detail: 1 });
    advance(500 + COLLAPSE_MARGIN_MS + 10);
    expect(panel()).not.toBeNull();
    advance(650 - 500);
    expect(panel()).toBeNull();
  });

  it("系统双击时间 650ms、两下相隔 600ms 的慢双击：面板全程没有收起过，连接只执行一次", () => {
    setDoubleClickMsForTests(650);
    const seen: (number | null)[] = [];
    render(<HostInfoProbe seen={seen} />);
    const actions = setup(true);
    const el = leaf("web-1");
    fireEvent.click(el, { detail: 1 });
    expect(panel()).not.toBeNull();
    seen.length = 0;

    fireEvent.click(el, { detail: 1 });
    advance(600);
    fireEvent.click(el, { detail: 2 });
    fireEvent.doubleClick(el, { detail: 2 });
    advance(collapseDelayMs() + 10);

    expect(seen.every((id) => id === 1)).toBe(true);
    expect(panel()).not.toBeNull();
    expect(actions.connect).toHaveBeenCalledTimes(1);
  });

  it("有跳板机 / 指定代理时多出对应的行，显示名字", () => {
    setup(true);
    fireEvent.click(leaf("app-3"));
    expect(panel()?.textContent).toContain("跳板机");
    expect(panel()?.textContent).toContain("web-1");
    expect(panel()?.textContent).toContain("代理");
    expect(panel()?.textContent).toContain("office-socks");
  });

  it("没有跳板机 / 代理就没有这两行；不出现任何凭据字样", () => {
    setup(true);
    fireEvent.click(leaf("web-1"));
    expect(panel()?.textContent).not.toContain("跳板机");
    expect(panel()?.textContent).not.toContain("代理");
    expect(panel()?.textContent).not.toMatch(/密码|私钥|口令/);
  });

  it("× 收起；地址旁的按钮复制地址", () => {
    const actions = setup(true);
    fireEvent.click(leaf("db-2"));

    fireEvent.click(screen.getByLabelText("复制地址"));
    expect(actions.copy).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }), "address");

    fireEvent.click(screen.getByLabelText("收起属性面板"));
    expect(panel()).toBeNull();
  });

  it("展开状态只在本次运行内记住：树卸载重挂后还是那一台", () => {
    setup(true);
    fireEvent.click(leaf("web-1"));
    cleanup();
    setup(true);
    expect(panel()?.textContent).toContain("web-1");
  });

  it("主机页（不传 infoPanel）：单击只走原来的选择，不出面板", () => {
    const actions = setup(false);
    fireEvent.click(leaf("web-1"));
    expect(actions.select).toHaveBeenCalledTimes(1);
    expect(panel()).toBeNull();
    fireEvent.doubleClick(leaf("web-1"));
    expect(actions.connect).toHaveBeenCalledTimes(1);
  });
});

describe("hostInfoRows", () => {
  it("跟随默认代理的主机不显示代理行；跳板机已被删了也不报错", () => {
    const rows = hostInfoRows(
      host(9, { proxy_mode: "default", proxy_id: null, jump_host_id: 99 }),
      [],
      [proxy],
    );
    expect(rows.map((row) => row.key)).toEqual(["name", "address", "user", "protocol", "port"]);
  });

  it("地址里带方括号的老 IPv6 去掉括号", () => {
    const rows = hostInfoRows(host(9, { host: "[2001:db8::9]" }), [], []);
    expect(rows.find((row) => row.key === "address")?.value).toBe("2001:db8::9");
  });
});
