/** @vitest-environment jsdom */
//! 「未分组」改名之后（UI-14）：树里没有「未分组」行，不分组的主机直接排在所有根分组下面；
//! 换分组 / 拖放 / 右键菜单里的文字改叫「顶层」。开关关着时和原来一样（其他测试文件负责）。

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
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import VaultView from "./VaultView";

const group = (id: number, name: string, parent_id: number | null = null, sort = 0): Group => ({
  id,
  name,
  parent_id,
  sort,
});
const host = (id: number, name: string, group_id: number | null, sort = 0) =>
  ({
    id,
    group_id,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags: [],
    sort,
  }) as unknown as Host;

// Alpha(1)：a1 a2；Beta(2)：b1；根层（没分组）：u1 u2 u3
const baseGroups = [group(1, "Alpha"), group(2, "Beta")];
const baseHosts = [
  host(1, "a1", 1),
  host(2, "a2", 1),
  host(4, "b1", 2),
  host(6, "u1", null),
  host(7, "u2", null),
  host(8, "u3", null),
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
  onOpenIdentities: vi.fn(),
  onOpenProxies: vi.fn(),
  onOpenForwards: vi.fn(),
  onNotice: vi.fn(),
});

let reorderHosts: ReturnType<typeof vi.fn>;
let resetOrder: ReturnType<typeof vi.fn>;
let saveHost: ReturnType<typeof vi.fn>;

function seed(groups: Group[] = baseGroups, hosts: Host[] = baseHosts) {
  reorderHosts = vi.fn(async () => {});
  resetOrder = vi.fn(async () => {});
  saveHost = vi.fn(async () => ({}) as Host);
  useVault.setState({
    groups,
    hosts,
    load: async () => {},
    reorderHosts,
    reorderGroups: vi.fn(async () => {}),
    resetOrder,
    saveHost,
  } as never);
}

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  seed();
  useSettings.setState({
    "layout.vault_expanded": "g1,g2",
    "layout.vault_sidebar_collapsed": false,
    "layout.vault_view_mode": "grid",
    vault_ungrouped_flat: true,
  });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const isTreeRow = (el: Element) =>
  el.getAttribute("data-section") === "tree-host" ||
  el.hasAttribute("data-order-group") ||
  el.textContent?.startsWith("全部主机") ||
  el.textContent?.startsWith("未分组");
const treeRows = () =>
  [...document.querySelectorAll('[data-section="vault-sidebar"] button')]
    .filter(isTreeRow)
    .map((el) => el.textContent ?? "");
const hostBtn = (name: string) =>
  [...document.querySelectorAll('[data-section="tree-host"]')].find((el) => el.textContent === name) as HTMLElement;
const allRow = () =>
  [...document.querySelectorAll('[data-section="vault-sidebar"] button')].find((el) =>
    el.textContent?.startsWith("全部主机"),
  ) as HTMLElement;
const action = (name: "up" | "down" | "reset") =>
  document.querySelector(`[data-order-action="${name}"]`) as HTMLElement;
const disabled = (el: HTMLElement) => el.getAttribute("aria-disabled") === "true";
const dropLines = () => [...document.querySelectorAll('[data-section="drop-line"]')];

async function openMenu(el: HTMLElement) {
  await act(async () => fireEvent.contextMenu(el));
}

/** 按下 `from`，指针移到 `target`（矩形 top 100 / 高 28）的 y 位置。拖动一直保持，直到 release。 */
async function dragOver(from: HTMLElement, target: HTMLElement, y: number) {
  const original = document.elementFromPoint;
  document.elementFromPoint = () => target;
  target.getBoundingClientRect = () => ({ top: 100, height: 28, bottom: 128, left: 0, right: 200, width: 200 }) as DOMRect;
  fireEvent.pointerDown(from, { button: 0, clientX: 10, clientY: 300 });
  await act(async () => {
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 40, clientY: y }));
  });
  return () => {
    document.elementFromPoint = original;
  };
}
async function release() {
  await act(async () => {
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 40, clientY: 0 }));
  });
}
const ghostText = () => document.querySelector('[data-section="host-drag-ghost"]')?.textContent ?? "";

describe("平铺模式的树", () => {
  it("没有「未分组」行；根层主机排在所有根分组（和它们的主机）下面", () => {
    render(<VaultView {...props()} />);
    expect(screen.queryByText("未分组")).toBeNull();
    expect(document.querySelector('[data-drop-group="none"]:not([data-section="tree-host"])')).toBeNull();
    expect(treeRows()).toEqual(["全部主机6", "Alpha2", "a1", "a2", "Beta1", "b1", "u1", "u2", "u3"]);
  });

  it("根层主机和根分组同一缩进、没有连接线；分组下的主机仍然缩进", () => {
    render(<VaultView {...props()} />);
    const root = hostBtn("u1");
    const alpha = document.querySelector('[data-order-group="1"]') as HTMLElement;
    expect(root.style.paddingLeft).toBe("20px"); // 4 + 0 * 14 + 16
    expect(alpha.style.paddingLeft).toBe("4px");
    expect(root.hasAttribute("data-tree-line")).toBe(false);
    expect(root.style.backgroundImage).toBe("");
    expect(hostBtn("a1").getAttribute("data-tree-line")).toBe("branch");
  });

  it("全是根层主机（没有任何分组）也照样显示，不需要展开", () => {
    seed([], [host(6, "u1", null), host(7, "u2", null)]);
    useSettings.setState({ "layout.vault_expanded": "" });
    render(<VaultView {...props()} />);
    expect(treeRows()).toEqual(["全部主机2", "u1", "u2"]);
  });

  it("根层主机按 sort 在前、其余按名字（UI-9 的同一层规则）", () => {
    seed(baseGroups, [host(6, "u1", null), host(7, "u2", null, 1), host(8, "u3", null, 2)]);
    render(<VaultView {...props()} />);
    expect(treeRows().slice(-3)).toEqual(["u2", "u3", "u1"]);
  });

  it("搜索：根层主机照常参与过滤，「未分组」这个名字搜不到", async () => {
    render(<VaultView {...props()} />);
    const box = screen.getByLabelText("搜索主机树");
    await act(async () => fireEvent.change(box, { target: { value: "u2" } }));
    expect(treeRows()).toEqual(["全部主机1", "u2"]);
    await act(async () => fireEvent.change(box, { target: { value: "未分组" } }));
    expect(screen.getByText("没有匹配的主机")).toBeTruthy();
  });

  it("搜索 ↑ ↓ 的顺序：根分组里的主机在前，根层主机在最后", async () => {
    const p = props();
    render(<VaultView {...p} />);
    const box = screen.getByLabelText("搜索主机树");
    await act(async () => fireEvent.change(box, { target: { value: "1" } }));
    // 结果顺序 a1、b1（分组里）、u1（根层）：↓ 三次走到根层的 u1，回车连它
    for (let i = 0; i < 3; i += 1) await act(async () => fireEvent.keyDown(box, { key: "ArrowDown" }));
    await act(async () => fireEvent.keyDown(box, { key: "Enter" }));
    expect(p.onOpenHost).toHaveBeenCalledTimes(1);
    expect((p.onOpenHost.mock.calls[0][0] as Host).name).toBe("u1");
  });

  it("开关关着时还是原来的「未分组」行", () => {
    useSettings.setState({ vault_ungrouped_flat: false, "layout.vault_expanded": "g1,g2,ungrouped" });
    render(<VaultView {...props()} />);
    expect(treeRows()).toEqual(["全部主机6", "Alpha2", "a1", "a2", "Beta1", "b1", "未分组3", "u1", "u2", "u3"]);
    expect(hostBtn("u1").style.paddingLeft).toBe("34px");
  });
});

describe("平铺模式的右键菜单", () => {
  it("「移动到分组」第一项叫「顶层（不分组）」，根层主机在它上面打勾；没有「未分组」", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u1"));
    await act(async () => fireEvent.click(screen.getByText("移动到分组")));
    const target = document.querySelector('[data-group-target="none"]') as HTMLElement;
    expect(target.textContent).toBe("顶层（不分组）");
    expect(disabled(target)).toBe(true);
    expect(screen.queryByText("未分组")).toBeNull();
  });

  it("从分组里移到顶层", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a1"));
    await act(async () => fireEvent.click(screen.getByText("移动到分组")));
    await act(async () => (document.querySelector('[data-group-target="none"]') as HTMLElement).click());
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1, group_id: null }));
  });

  it("根层主机上移 / 下移：按根层这一层整层重新编号；头尾置灰", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u2"));
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenCalledWith([7, 6, 8]);
    cleanup();
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u1"));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(false);
  });

  it("根层主机右键有「按名字重新排序（顶层主机）」，作用同 order_reset(None)；分组里的主机没有", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u1"));
    await act(async () => screen.getByText("按名字重新排序（顶层主机）").click());
    expect(resetOrder).toHaveBeenCalledWith(null);
    cleanup();
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a1"));
    expect(screen.queryByText("按名字重新排序（顶层主机）")).toBeNull();
  });

  it("开关关着时主机右键没有「按名字重新排序（顶层主机）」", async () => {
    useSettings.setState({ vault_ungrouped_flat: false, "layout.vault_expanded": "g1,g2,ungrouped" });
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u1"));
    expect(screen.queryByText("按名字重新排序（顶层主机）")).toBeNull();
    expect(action("reset")).toBeNull();
  });
});

describe("平铺模式的拖放", () => {
  it("拖到树的空白处 = 移到顶层，提示文字是「→ 顶层」", async () => {
    render(<VaultView {...props()} />);
    const blank = document.querySelector("[data-drop-blank]") as HTMLElement;
    const restore = await dragOver(hostBtn("a1"), blank, 110);
    try {
      expect(ghostText()).toBe("a1 → 顶层");
      expect(dropLines().length).toBe(0);
      await release();
    } finally {
      restore();
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1, group_id: null }));
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("拖到「全部主机」那一行（不是空白）什么都不做", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("a1"), allRow(), 110);
    try {
      expect(ghostText()).toBe("a1（拖到左边的分组上）");
      await release();
    } finally {
      restore();
    }
    expect(saveHost).not.toHaveBeenCalled();
  });

  it("分组里的主机拖到根层主机之间 = 移到顶层并插在那个位置", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("a1"), hostBtn("u2"), 105);
    try {
      expect(ghostText()).toBe("a1 → 顶层");
      await release();
    } finally {
      restore();
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1, group_id: null }));
    // 插在 u2 前面：u1 a1 u2 u3
    expect(reorderHosts).toHaveBeenCalledWith([6, 1, 7, 8]);
  });

  it("根层主机之间拖动调整顺序，不换分组", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("u3"), hostBtn("u1"), 105);
    try {
      expect(ghostText()).toBe("u3（调整顺序）");
      await release();
    } finally {
      restore();
    }
    expect(saveHost).not.toHaveBeenCalled();
    expect(reorderHosts).toHaveBeenCalledWith([8, 6, 7]);
  });

  it("开关关着时树没有空白处这个落点", () => {
    useSettings.setState({ vault_ungrouped_flat: false });
    render(<VaultView {...props()} />);
    expect(document.querySelector("[data-drop-blank]")).toBeNull();
  });
});

describe("平铺模式的文字", () => {
  it("列表视图的分组列：没分组的主机显示「顶层」", () => {
    useSettings.setState({ "layout.vault_view_mode": "list" });
    render(<VaultView {...props()} />);
    const rows = [...document.querySelectorAll('[data-section="host-row"]')];
    const u1 = rows.find((row) => row.textContent?.includes("u1")) as HTMLElement;
    expect(u1.textContent).toContain("顶层");
    expect(u1.textContent).not.toContain("未分组");
    const a1 = rows.find((row) => row.textContent?.includes("a1")) as HTMLElement;
    expect(a1.textContent).toContain("Alpha");
  });

  it("列表视图开关关着时仍是「未分组」", () => {
    useSettings.setState({ "layout.vault_view_mode": "list", vault_ungrouped_flat: false });
    render(<VaultView {...props()} />);
    const u1 = [...document.querySelectorAll('[data-section="host-row"]')].find((row) =>
      row.textContent?.includes("u1"),
    ) as HTMLElement;
    expect(u1.textContent).toContain("未分组");
  });
});
