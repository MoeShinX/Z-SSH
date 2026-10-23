/** @vitest-environment jsdom */
//! 主机页左边的树（像 Xshell）：分组可以展开 / 收起，主机挂在分组下面，双击连接。

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
import VaultView, { flattenGroups } from "./VaultView";
import { UNGROUPED_KEY } from "../../state/hostTree";

const group: Group = { id: 2, name: "Wap-PVE", parent_id: null, sort: 0 };
const host = (id: number, name: string, group_id: number | null) =>
  ({
    id,
    group_id,
    name,
    host: `${name.toLowerCase().replace(/\s/g, "-")}.example.internal`,
    port: 22,
    username: "root",
    tags: [],
  }) as unknown as Host;

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
  onOpenIdentities: vi.fn(),
  onOpenProxies: vi.fn(),
  onOpenForwards: vi.fn(),
  onNotice: vi.fn(),
});

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  useVault.setState({
    groups: [group],
    hosts: [host(1, "CA1 PVE", 2), host(2, "DE1 PVE", 2), host(3, "loose", null)],
    load: async () => {},
  } as never);
  useSettings.setState({ "layout.vault_expanded": "", "layout.vault_sidebar_collapsed": false });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const treeHosts = () =>
  [...document.querySelectorAll('[data-section="tree-host"]')].map((el) => el.textContent);

describe("主机树", () => {
  it("分组默认收起；点箭头展开，主机挂在下面，再点收起", async () => {
    render(<VaultView {...props()} />);
    expect(treeHosts()).toEqual([]);
    const [groupToggle] = document.querySelectorAll('[data-section="tree-toggle"]');
    await act(async () => fireEvent.click(groupToggle));
    expect(treeHosts()).toEqual(["CA1 PVE", "DE1 PVE"]);
    expect(useSettings.getState()["layout.vault_expanded"]).toBe("g2");
    await act(async () => fireEvent.click(document.querySelectorAll('[data-section="tree-toggle"]')[0]));
    expect(treeHosts()).toEqual([]);
  });

  it("单击只展开底部的属性面板、不打开详情；右键「编辑」才打开详情；双击连接", async () => {
    useSettings.setState({ "layout.vault_expanded": "g2" });
    const handlers = props();
    render(<VaultView {...handlers} />);
    const [first] = document.querySelectorAll('[data-section="tree-host"]');
    fireEvent.click(first);
    expect(handlers.onSelectHost).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="host-info"]')?.textContent).toContain("CA1 PVE");
    await act(async () => fireEvent.contextMenu(first));
    await act(async () => screen.getByText("编辑").click());
    expect(handlers.onSelectHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
    fireEvent.doubleClick(first);
    expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
  });

  it("搜索时有匹配主机的分组自动展开", async () => {
    render(<VaultView {...props()} />);
    // 树自己的搜索框（UI-7）；右侧大搜索框不再驱动树
    const search = screen.getByLabelText("搜索主机树");
    await act(async () => fireEvent.change(search, { target: { value: "de1" } }));
    expect(treeHosts()).toEqual(["DE1 PVE"]);
  });

  it("侧边栏能收起成一列图标，再展开回来", async () => {
    render(<VaultView {...props()} />);
    expect(document.querySelector('[data-section="vault-sidebar"]')).not.toBeNull();
    await act(async () => screen.getByTitle("收起侧边栏").click());
    expect(useSettings.getState()["layout.vault_sidebar_collapsed"]).toBe(true);
    expect(document.querySelector('[data-section="vault-sidebar"]')).toBeNull();
    expect(treeHosts()).toEqual([]);
    await act(async () => screen.getByTitle("展开侧边栏").click());
    expect(document.querySelector('[data-section="vault-sidebar"]')).not.toBeNull();
  });
});

describe("右键移动到分组", () => {
  it("分组按树的顺序摊平，子组跟在父组后面", () => {
    const groups: Group[] = [
      { id: 1, name: "生产", parent_id: null, sort: 0 },
      { id: 2, name: "测试", parent_id: null, sort: 1 },
      { id: 3, name: "香港", parent_id: 1, sort: 0 },
    ];
    expect(flattenGroups(groups).map(({ group, depth }) => `${group.name}:${depth}`)).toEqual([
      "生产:0",
      "香港:1",
      "测试:0",
    ]);
  });

  it("右键主机 → 移动到分组 → 选分组，存成新分组；当前所在的点不了", async () => {
    const saveHost = vi.fn(async () => ({}) as Host);
    useVault.setState({ saveHost } as never);
    useSettings.setState({ "layout.vault_expanded": UNGROUPED_KEY });
    render(<VaultView {...props()} />);
    const loose = [...document.querySelectorAll('[data-section="tree-host"]')].find(
      (el) => el.textContent === "loose",
    ) as HTMLElement;
    await act(async () => fireEvent.contextMenu(loose));
    const trigger = screen.getByText("移动到分组");
    await act(async () => fireEvent.keyDown(trigger, { key: "ArrowRight" }));
    const none = document.querySelector('[data-group-target="none"]') as HTMLElement;
    expect(none.getAttribute("aria-disabled")).toBe("true");
    const target = document.querySelector('[data-group-target="2"]') as HTMLElement;
    await act(async () => fireEvent.click(target));
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 3, name: "loose", group_id: 2 }));
  });
});

describe("拖主机换分组", () => {
  it("按住树里的主机拖到分组行上松手：移过去；拖动结束那次单击不算选中", async () => {
    const saveHost = vi.fn(async () => ({}) as Host);
    useVault.setState({ saveHost } as never);
    useSettings.setState({ "layout.vault_expanded": UNGROUPED_KEY });
    const handlers = props();
    render(<VaultView {...handlers} />);
    const loose = [...document.querySelectorAll('[data-section="tree-host"]')].find(
      (el) => el.textContent === "loose",
    ) as HTMLElement;
    const groupRow = document.querySelector('[data-drop-group="2"]') as HTMLElement;
    const original = document.elementFromPoint;
    document.elementFromPoint = () => groupRow;
    try {
      fireEvent.pointerDown(loose, { button: 0, clientX: 10, clientY: 200 });
      await act(async () => {
        window.dispatchEvent(new MouseEvent("pointermove", { clientX: 40, clientY: 60 }));
      });
      expect(document.querySelector('[data-section="host-drag-ghost"]')?.textContent).toContain("loose → Wap-PVE");
      // 真实浏览器里松手和随后的 click 在同一轮事件里连着发：中间不能让出去
      await act(async () => {
        window.dispatchEvent(new MouseEvent("pointerup", { clientX: 40, clientY: 60 }));
        fireEvent.click(loose);
      });
    } finally {
      document.elementFromPoint = original;
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 3, group_id: 2 }));
    expect(handlers.onSelectHost).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="host-drag-ghost"]')).toBeNull();
  });

  it("没拖过门槛就是普通单击，不移动", async () => {
    const saveHost = vi.fn(async () => ({}) as Host);
    useVault.setState({ saveHost } as never);
    useSettings.setState({ "layout.vault_expanded": UNGROUPED_KEY });
    const handlers = props();
    render(<VaultView {...handlers} />);
    const loose = [...document.querySelectorAll('[data-section="tree-host"]')].find(
      (el) => el.textContent === "loose",
    ) as HTMLElement;
    fireEvent.pointerDown(loose, { button: 0, clientX: 10, clientY: 200 });
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 12, clientY: 201 }));
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 12, clientY: 201 }));
    fireEvent.click(loose);
    expect(saveHost).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="host-info"]')?.textContent).toContain("loose");
  });
});

describe("右键「未分组」", () => {
  it("有新建分组和重命名（建成分组）；重命名带上把未分组主机收进去的标记", async () => {
    const handlers = props();
    render(<VaultView {...handlers} />);
    const row = document.querySelector('[data-drop-group="none"]:not([data-section="tree-host"])') as HTMLElement;
    await act(async () => fireEvent.contextMenu(row));
    expect(screen.getByText("新建分组")).toBeTruthy();
    await act(async () => fireEvent.click(screen.getByText("重命名（建成分组）…")));
    expect(handlers.onNewGroup).toHaveBeenCalledWith(null, { adoptUngrouped: true });
  });
});

describe("主机树的虚线连接线", () => {
  const A: Group = { id: 2, name: "A", parent_id: null, sort: 0 };
  const B: Group = { id: 3, name: "B", parent_id: 2, sort: 0 };
  const C: Group = { id: 4, name: "C", parent_id: 2, sort: 1 };
  /** 树里带连接线的行，按显示顺序：[标签, 形状, 穿过的祖先层]。 */
  const lines = () =>
    [...document.querySelectorAll('[data-section="vault-sidebar"] [data-tree-line]')].map((el) => [
      el.querySelector("span.truncate")?.textContent,
      el.getAttribute("data-tree-line"),
      el.getAttribute("data-tree-trail"),
    ]);

  function setup(hosts: Host[], expanded = "g2,g3,g4") {
    useVault.setState({ groups: [A, B, C], hosts } as never);
    useSettings.setState({ "layout.vault_expanded": expanded });
    render(<VaultView {...props()} />);
  }

  it("每一层都有连接线；最后一个子项是 └，祖先后面还有兄弟时竖线穿过子孙行", () => {
    setup([host(10, "a-host", 2), host(11, "b1", 3), host(12, "b2", 3), host(13, "loose", null)], "g2,g3,g4," + UNGROUPED_KEY);
    expect(lines()).toEqual([
      ["B", "branch", ""], // A 的第一个子分组，后面还有 C 和主机
      ["b1", "branch", "1"], // B 后面还有兄弟：A 下的竖线穿过
      ["b2", "last", "1"],
      ["C", "branch", ""], // 后面还有 a-host
      ["a-host", "last", ""],
      ["loose", "last", ""], // 未分组下的主机
    ]);
  });

  it("没有主机时最后一个子分组是 └，它的子孙行不再有穿过的竖线", () => {
    setup([host(11, "b1", 3), host(14, "c1", 4)]);
    expect(lines()).toEqual([
      ["B", "branch", ""],
      ["b1", "last", "1"],
      ["C", "last", ""],
      ["c1", "last", "0"],
    ]);
  });

  it("顶层节点（全部主机 / 分组 / 未分组）没有连接线；收起的分组不画子项", () => {
    setup([host(10, "a-host", 2)], "");
    expect(lines()).toEqual([]);
  });

  it("带连接线的行样式是背景图，高亮用的类名和拖放标记不变", () => {
    setup([host(10, "a-host", 2)], "g2");
    const row = document.querySelector<HTMLElement>('[data-tree-line][data-drop-group="3"]')!;
    expect(row.className).toContain("hover:bg-accent/50");
    expect(row.className).toContain("rounded-md");
    expect(row.style.paddingLeft).toBe("18px");
    const leaf = document.querySelector<HTMLElement>('[data-section="tree-host"]')!;
    expect(leaf.dataset.treeLine).toBe("last");
    expect(leaf.style.paddingLeft).toBe("34px");
  });

  it("收起成图标栏时不画线", async () => {
    setup([host(10, "a-host", 2)], "");
    await act(async () => screen.getByTitle("收起侧边栏").click());
    expect(document.querySelectorAll("[data-tree-line]")).toHaveLength(0);
  });
});
