/** @vitest-environment jsdom */
//! 主机树顶部的搜索框（UI-7）：输入即过滤，分组自动展开，高亮，键盘 ↑ ↓ Enter Esc。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

import type { Group, Host } from "../../api/vault";
import { useSettings } from "../../stores/settings";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useVault } from "../../stores/vault";
import VaultView, { HostTreeAside, type HostActions } from "./VaultView";

const groups: Group[] = [
  { id: 1, name: "Prod", parent_id: null, sort: 1 },
  { id: 2, name: "Lab", parent_id: null, sort: 2 },
  { id: 3, name: "Asia", parent_id: 1, sort: 0 },
];
const host = (id: number, name: string, group_id: number | null, extra: Partial<Host> = {}) =>
  ({
    id,
    group_id,
    name,
    host: `10.0.0.${id}`,
    port: 22,
    username: "ops",
    tags: [],
    ...extra,
  }) as unknown as Host;
const hosts: Host[] = [
  host(1, "web-a", 1),
  host(2, "web-b", 3, { tags: ["edge"] }),
  host(3, "db-main", 3, { username: "dba", host: "10.0.0.33" }),
  host(4, "lab-box", 2),
  host(5, "Loose One", null),
];

const props = () => ({
  onOpenHost: vi.fn(),
  onOpenSftp: vi.fn(),
  onQuickConnect: vi.fn(),
  onSelectHost: vi.fn(),
  onNewHost: vi.fn(),
  onNewGroup: vi.fn(),
  onEditGroup: vi.fn(),
  onDeleteGroup: vi.fn(),
  onDeleteHost: vi.fn(),
  onOpenProxies: vi.fn(),
  onOpenForwards: vi.fn(),
  onNotice: vi.fn(),
});

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  useVault.setState({ groups, hosts, load: async () => {} } as never);
  useSettings.setState({ "layout.vault_expanded": "", "layout.vault_sidebar_collapsed": false });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const box = () => screen.getByLabelText("搜索主机树") as HTMLInputElement;
const type = async (value: string) => {
  await act(async () => fireEvent.change(box(), { target: { value } }));
};
const press = async (key: string) => {
  await act(async () => fireEvent.keyDown(box(), { key }));
};
const leaves = () =>
  [...document.querySelectorAll('[data-section="tree-host"]')].map((el) => el.querySelector("span.truncate")?.textContent);
const activeLeaves = () =>
  [...document.querySelectorAll('[data-search-active="true"]')].map((el) => el.querySelector("span.truncate")?.textContent);
/** 树里每一行（分组行、主机行）的文字，分组行是「名字 + 数字」。 */
const rowsOf = () =>
  [...document.querySelectorAll('[data-section="vault-sidebar"] button')]
    .filter((el) => el.className.includes("h-7"))
    .map((el) => el.textContent);

describe("主机树搜索框", () => {
  it("在「新建主机」和「全部主机」之间，占满树的宽度", () => {
    render(<VaultView {...props()} />);
    const aside = document.querySelector('[data-section="vault-sidebar"]') as HTMLElement;
    const children = [...aside.children];
    const newHost = children.findIndex((el) => el.textContent?.includes("新建主机"));
    const search = children.findIndex((el) => el.contains(box()));
    const scroll = children.findIndex((el) => el.textContent?.includes("全部主机"));
    expect(newHost).toBeGreaterThanOrEqual(0);
    expect(search).toBe(newHost + 1);
    expect(scroll).toBe(search + 1);
    expect(box().className).toContain("w-full");
  });

  it("没输入时树照旧：分组收起，不出现 × 和提示", () => {
    render(<VaultView {...props()} />);
    expect(leaves()).toEqual([]);
    expect(screen.queryByLabelText("清空搜索")).toBeNull();
    expect(screen.queryByText("没有匹配的主机")).toBeNull();
  });

  it("按名称、地址、用户名、标签过滤，不区分大小写", async () => {
    render(<VaultView {...props()} />);
    await type("WEB");
    expect(leaves()).toEqual(["web-b", "web-a"]);
    await type("10.0.0.33");
    expect(leaves()).toEqual(["db-main"]);
    await type("DBA");
    expect(leaves()).toEqual(["db-main"]);
    await type("Edge");
    expect(leaves()).toEqual(["web-b"]);
    await type("loose");
    expect(leaves()).toEqual(["Loose One"]);
  });

  it("只显示匹配的主机和它所在的分组路径，分组自动展开", async () => {
    render(<VaultView {...props()} />);
    await type("db-main");
    expect(leaves()).toEqual(["db-main"]);
    const rows = rowsOf();
    expect(rows.some((text) => text?.startsWith("Prod"))).toBe(true);
    expect(rows.some((text) => text?.startsWith("Asia"))).toBe(true);
    expect(rows.some((text) => text?.startsWith("Lab"))).toBe(false);
    expect(rows.some((text) => text?.startsWith("未分组"))).toBe(false);
  });

  it("不改用户自己的展开状态，清空后恢复原样", async () => {
    useSettings.setState({ "layout.vault_expanded": "g2" });
    render(<VaultView {...props()} />);
    expect(leaves()).toEqual(["lab-box"]);
    await type("db-main");
    expect(leaves()).toEqual(["db-main"]);
    // 搜索时点箭头也不去改存着的展开状态
    await act(async () => fireEvent.click(document.querySelectorAll('[data-section="tree-toggle"]')[0]));
    expect(useSettings.getState()["layout.vault_expanded"]).toBe("g2");
    await act(async () => fireEvent.click(screen.getByLabelText("清空搜索")));
    expect(box().value).toBe("");
    expect(leaves()).toEqual(["lab-box"]);
    expect(useSettings.getState()["layout.vault_expanded"]).toBe("g2");
  });

  it("分组名匹配时显示该分组（含子分组）下的全部主机", async () => {
    render(<VaultView {...props()} />);
    await type("prod");
    expect(leaves()).toEqual(["db-main", "web-b", "web-a"]);
    await type("asia");
    expect(leaves()).toEqual(["db-main", "web-b"]);
  });

  it("匹配到的文字高亮，不区分大小写", async () => {
    render(<VaultView {...props()} />);
    await type("OOSE");
    const marks = [...document.querySelectorAll('[data-section="tree-host"] mark')];
    expect(marks.map((el) => el.textContent)).toEqual(["oose"]);
    await type("prod");
    const groupMarks = [...document.querySelectorAll('[data-section="vault-sidebar"] mark')].map((el) => el.textContent);
    expect(groupMarks).toEqual(["Prod"]);
  });

  it("名字没匹配、靠地址 / 用户名 / 标签匹配时，在行尾标出匹配的那一项", async () => {
    render(<VaultView {...props()} />);
    await type("edge");
    const hint = document.querySelector('[data-section="tree-match-hint"]');
    expect(hint?.textContent).toBe("edge");
    expect(hint?.querySelector("mark")?.textContent).toBe("edge");
    await type("web-b");
    expect(document.querySelector('[data-section="tree-match-hint"]')).toBeNull();
  });

  it("没有结果时显示「没有匹配的主机」", async () => {
    render(<VaultView {...props()} />);
    await type("zzz-none");
    expect(screen.getByText("没有匹配的主机")).toBeTruthy();
    expect(leaves()).toEqual([]);
    expect(screen.queryByText("全部主机")).toBeNull();
    await type("");
    expect(screen.queryByText("没有匹配的主机")).toBeNull();
    expect(screen.getByText("全部主机")).toBeTruthy();
  });

  it("分组计数显示过滤后的数量，包括子分组", async () => {
    render(<VaultView {...props()} />);
    expect(rowsOf().slice(0, 4)).toEqual(["全部主机5", "Prod3", "Lab1", "未分组1"]);
    await type("web");
    // 全部主机 = 2；Prod 里（含 Asia）有 2 台匹配；Asia 里 1 台
    expect(rowsOf()).toEqual(["全部主机2", "Prod2", "Asia1", "web-b", "web-a"]);
  });

  it("↑ ↓ 在结果间移动高亮，到头不循环；Enter 连接高亮的那台", async () => {
    const handlers = props();
    render(<VaultView {...handlers} />);
    await type("web");
    expect(activeLeaves()).toEqual([]);
    await press("ArrowDown");
    expect(activeLeaves()).toEqual(["web-b"]);
    await press("ArrowDown");
    expect(activeLeaves()).toEqual(["web-a"]);
    await press("ArrowDown");
    expect(activeLeaves()).toEqual(["web-a"]);
    await press("ArrowUp");
    expect(activeLeaves()).toEqual(["web-b"]);
    await press("ArrowUp");
    expect(activeLeaves()).toEqual(["web-b"]);
    await press("ArrowDown");
    await press("Enter");
    expect(handlers.onOpenHost).toHaveBeenCalledTimes(1);
    expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
  });

  it("没有高亮时 Enter 连接结果里的第一台；没有结果时什么都不做", async () => {
    const handlers = props();
    render(<VaultView {...handlers} />);
    await type("web");
    await press("Enter");
    expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }));
    await type("zzz-none");
    await press("Enter");
    expect(handlers.onOpenHost).toHaveBeenCalledTimes(1);
  });

  it("改了搜索词，高亮清掉", async () => {
    render(<VaultView {...props()} />);
    await type("web");
    await press("ArrowDown");
    expect(activeLeaves()).toEqual(["web-b"]);
    await type("web-");
    expect(activeLeaves()).toEqual([]);
  });

  it("Esc：有内容先清空，已空时输入框失焦", async () => {
    render(<VaultView {...props()} />);
    box().focus();
    await type("web");
    await press("Escape");
    expect(box().value).toBe("");
    expect(leaves()).toEqual([]);
    expect(document.activeElement).toBe(box());
    await press("Escape");
    expect(document.activeElement).not.toBe(box());
  });

  it("树里单击 / 双击 / 右键的行为不变", async () => {
    const handlers = props();
    render(<VaultView {...handlers} />);
    await type("lab");
    const [row] = document.querySelectorAll('[data-section="tree-host"]');
    // 单击展开属性面板（不打开详情）
    fireEvent.click(row);
    expect(handlers.onSelectHost).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="host-info"]')).not.toBeNull();
    fireEvent.doubleClick(row);
    expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 4 }));
    await act(async () => fireEvent.contextMenu(row));
    expect(screen.getByText("移动到分组")).toBeTruthy();
  });

  it("主机页右侧的大搜索框和树的搜索框互不影响", async () => {
    const handlers = props();
    render(<VaultView {...handlers} />);
    const big = screen.getByPlaceholderText(/搜索主机；/) as HTMLInputElement;
    await act(async () => fireEvent.change(big, { target: { value: "lab" } }));
    // 大搜索框过滤右边的卡片，树不跟着变
    expect(box().value).toBe("");
    expect(leaves()).toEqual([]);
    await type("web");
    expect(big.value).toBe("lab");
    expect(leaves()).toEqual(["web-b", "web-a"]);
    // 大搜索框的快速连接照旧
    await act(async () => fireEvent.change(big, { target: { value: "ops@10.0.0.9:22" } }));
    await act(async () => fireEvent.keyDown(big, { key: "Enter" }));
    expect(handlers.onQuickConnect).toHaveBeenCalledWith("ops@10.0.0.9:22");
    expect(handlers.onOpenHost).not.toHaveBeenCalled();
  });

  it("搜索框按输入法规则属于英文区：自己和祖先都没有 data-ime 标记", () => {
    render(<VaultView {...props()} />);
    expect(box().closest('[data-ime="native"]')).toBeNull();
  });
});

describe("终端界面的主机树（同一个组件）", () => {
  const actions = (): HostActions => ({
    select: vi.fn(),
    connect: vi.fn(),
    openSftp: vi.fn(),
    remove: vi.fn(),
    copy: vi.fn(),
    copyPassword: vi.fn(),
    copyInfo: vi.fn(),
  });
  const noop = () => {};
  const aside = (hostActions: HostActions) => (
    <HostTreeAside
      selectedGroup="all"
      onSelectGroup={noop}
      selectedHostId={null}
      hostActions={hostActions}
      infoPanel
      onNewHost={noop}
      onNewGroup={noop}
      onEditGroup={noop}
      onDeleteGroup={noop}
      collapsedStyle="hidden"
    />
  );

  it("也有搜索框，Enter 连接", async () => {
    const hostActions = actions();
    render(aside(hostActions));
    await type("lab");
    expect(leaves()).toEqual(["lab-box"]);
    await press("Enter");
    expect(hostActions.connect).toHaveBeenCalledWith(expect.objectContaining({ id: 4 }));
  });

  it("两个树实例各记各的搜索词，卸载后不保留", async () => {
    const first = render(aside(actions()));
    await type("lab");
    first.unmount();
    render(aside(actions()));
    expect(box().value).toBe("");
    expect(leaves()).toEqual([]);
  });
});
