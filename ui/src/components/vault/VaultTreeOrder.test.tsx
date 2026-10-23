/** @vitest-environment jsdom */
//! 主机树手动调整顺序（UI-9）：右键上移 / 下移 / 按名字重新排序，拖动插入线，搜索时禁用排序拖动。

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

// Alpha(1)：a1 a2 a3 + 子分组 Sub(3)；Beta(2)：b1 b2；未分组：u1 u2
const baseGroups = [group(1, "Alpha"), group(2, "Beta"), group(3, "Sub", 1)];
const baseHosts = [
  host(1, "a1", 1),
  host(2, "a2", 1),
  host(3, "a3", 1),
  host(4, "b1", 2),
  host(5, "b2", 2),
  host(6, "u1", null),
  host(7, "u2", null),
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
let reorderGroups: ReturnType<typeof vi.fn>;
let resetOrder: ReturnType<typeof vi.fn>;
let saveHost: ReturnType<typeof vi.fn>;

function seed(groups: Group[] = baseGroups, hosts: Host[] = baseHosts) {
  reorderHosts = vi.fn(async () => {});
  reorderGroups = vi.fn(async () => {});
  resetOrder = vi.fn(async () => {});
  saveHost = vi.fn(async () => ({}) as Host);
  useVault.setState({ groups, hosts, load: async () => {}, reorderHosts, reorderGroups, resetOrder, saveHost } as never);
}

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  seed();
  useSettings.setState({ "layout.vault_expanded": "g1,g2,ungrouped", "layout.vault_sidebar_collapsed": false });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const hostBtn = (name: string) =>
  [...document.querySelectorAll('[data-section="tree-host"]')].find((el) => el.textContent === name) as HTMLElement;
const groupBtn = (id: number) => document.querySelector(`[data-order-group="${id}"]`) as HTMLElement;
const treeHostNames = () => [...document.querySelectorAll('[data-section="tree-host"]')].map((el) => el.textContent);
const action = (name: "up" | "down" | "reset") =>
  document.querySelector(`[data-order-action="${name}"]`) as HTMLElement;
const disabled = (el: HTMLElement) => el.getAttribute("aria-disabled") === "true";
const dropLines = () => [...document.querySelectorAll('[data-section="drop-line"]')];

async function openMenu(el: HTMLElement) {
  await act(async () => fireEvent.contextMenu(el));
}

/** 按下 `from`，指针移到 `target` 的 y 位置（target 的矩形是 top 100 / 高 28）。拖动一直保持，直到 release。 */
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

describe("显示顺序", () => {
  it("默认（全是 0）按名字；调整过的 sort > 0 在前，新加的（0）排在后面", () => {
    useSettings.setState({ "layout.vault_expanded": "g1" });
    seed(baseGroups, [
      host(1, "a1", 1, 2),
      host(2, "a2", 1),
      host(3, "a3", 1, 1),
      host(8, "a0-new", 1),
      host(4, "b1", 2),
    ]);
    render(<VaultView {...props()} />);
    expect(treeHostNames()).toEqual(["a3", "a1", "a0-new", "a2"]);
  });

  it("分组也按 sort：Beta 排在 Alpha 前面", () => {
    seed([group(1, "Alpha", null, 2), group(2, "Beta", null, 1), group(3, "Sub", 1)]);
    render(<VaultView {...props()} />);
    const roots = [...document.querySelectorAll('[data-order-group][data-order-parent="root"]')].map((el) =>
      el.getAttribute("data-order-group"),
    );
    expect(roots).toEqual(["2", "1"]);
  });
});

describe("右键上移 / 下移", () => {
  it("主机上移：传整层的新顺序", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a2"));
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenCalledWith([2, 1, 3]);
  });

  it("主机下移", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a1"));
    await act(async () => action("down").click());
    expect(reorderHosts).toHaveBeenCalledWith([2, 1, 3]);
  });

  it("按当前显示顺序算整层（已经调整过的）", async () => {
    seed(baseGroups, [host(1, "a1", 1, 2), host(2, "a2", 1), host(3, "a3", 1, 1), host(4, "b1", 2)]);
    render(<VaultView {...props()} />);
    // 显示顺序 a3 a1 a2：a2 上移 → a3 a2 a1
    await openMenu(hostBtn("a2"));
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenCalledWith([3, 2, 1]);
  });

  it("这一层第一个的上移置灰、最后一个的下移置灰；别的层不受影响", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a1"));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(false);
    await act(async () => fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" }));
    cleanup();
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("a3"));
    expect(disabled(action("up"))).toBe(false);
    expect(disabled(action("down"))).toBe(true);
  });

  it("未分组的主机在自己那一层里调整", async () => {
    render(<VaultView {...props()} />);
    await openMenu(hostBtn("u2"));
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenCalledWith([7, 6]);
  });

  it("分组上移 / 下移：同一个父分组下整层的新顺序；头尾置灰", async () => {
    render(<VaultView {...props()} />);
    await openMenu(groupBtn(2));
    expect(disabled(action("down"))).toBe(true);
    await act(async () => action("up").click());
    expect(reorderGroups).toHaveBeenCalledWith([2, 1]);
  });

  it("子分组只和同一个父分组下的兄弟比较", async () => {
    render(<VaultView {...props()} />);
    // Sub 是 Alpha 下唯一的子分组：上移、下移都置灰
    await openMenu(groupBtn(3));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(true);
  });

  it("搜索时上移 / 下移置灰（过滤后的列表不连续）", async () => {
    render(<VaultView {...props()} />);
    await act(async () => fireEvent.change(screen.getByLabelText("搜索主机树"), { target: { value: "a" } }));
    await openMenu(hostBtn("a2"));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(true);
  });
});

describe("按名字重新排序", () => {
  it("分组右键：重置这个分组下直接的主机和子分组", async () => {
    render(<VaultView {...props()} />);
    await openMenu(groupBtn(1));
    await act(async () => action("reset").click());
    expect(resetOrder).toHaveBeenCalledWith(1);
  });

  it("「未分组」右键：只管未分组的主机", async () => {
    render(<VaultView {...props()} />);
    const row = document.querySelector('[data-drop-group="none"]:not([data-section="tree-host"])') as HTMLElement;
    await openMenu(row);
    await act(async () => screen.getByText("按名字重新排序").click());
    expect(resetOrder).toHaveBeenCalledWith(null);
  });
});

describe("拖动调整顺序", () => {
  it("同一层拖到另一台主机的上半部分：画插入线（前面），松手整层重新编号", async () => {
    render(<VaultView {...props()} />);
    const target = hostBtn("a1");
    const restore = await dragOver(hostBtn("a3"), target, 105);
    try {
      expect(dropLines().length).toBe(1);
      expect(target.querySelector('[data-section="drop-line"]')?.getAttribute("data-position")).toBe("before");
      await release();
    } finally {
      restore();
    }
    expect(reorderHosts).toHaveBeenCalledWith([3, 1, 2]);
    expect(saveHost).not.toHaveBeenCalled();
    expect(dropLines().length).toBe(0);
  });

  it("下半部分：插到它后面", async () => {
    render(<VaultView {...props()} />);
    const target = hostBtn("a1");
    const restore = await dragOver(hostBtn("a3"), target, 120);
    try {
      expect(target.querySelector('[data-section="drop-line"]')?.getAttribute("data-position")).toBe("after");
      await release();
    } finally {
      restore();
    }
    expect(reorderHosts).toHaveBeenCalledWith([1, 3, 2]);
  });

  it("位置没变（拖到原位）什么都不做", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("a1"), hostBtn("a2"), 105);
    try {
      await release();
    } finally {
      restore();
    }
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("拖到另一个分组里两台主机之间：先换分组，再按松手的位置给目标那层编号", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("u1"), hostBtn("b1"), 120);
    try {
      await release();
    } finally {
      restore();
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 6, group_id: 2 }));
    expect(reorderHosts).toHaveBeenCalledWith([4, 6, 5]);
  });

  it("拖到分组行上仍然是换分组（行高亮，没有插入线）", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(hostBtn("u1"), groupBtn(2), 110);
    try {
      expect(dropLines().length).toBe(0);
      await release();
    } finally {
      restore();
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 6, group_id: 2 }));
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("分组在同一层拖到另一个分组的上半部分：插到前面", async () => {
    render(<VaultView {...props()} />);
    const target = groupBtn(1);
    const restore = await dragOver(groupBtn(2), target, 105);
    try {
      expect(target.querySelector('[data-section="drop-line"]')?.getAttribute("data-position")).toBe("before");
      expect(document.querySelector('[data-section="host-drag-ghost"]')?.textContent).toContain("Beta");
      await release();
    } finally {
      restore();
    }
    expect(reorderGroups).toHaveBeenCalledWith([2, 1]);
  });

  it("分组不能拖进别的层：子分组拖到根分组上没有反应", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(groupBtn(3), groupBtn(2), 105);
    try {
      expect(dropLines().length).toBe(0);
      await release();
    } finally {
      restore();
    }
    expect(reorderGroups).not.toHaveBeenCalled();
    expect(saveHost).not.toHaveBeenCalled();
  });

  it("分组拖到主机行上没有反应", async () => {
    render(<VaultView {...props()} />);
    const restore = await dragOver(groupBtn(2), hostBtn("a1"), 105);
    try {
      expect(dropLines().length).toBe(0);
      await release();
    } finally {
      restore();
    }
    expect(reorderGroups).not.toHaveBeenCalled();
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("搜索框里有字时不能排序拖动（主机）：没有插入线，松手不调整", async () => {
    render(<VaultView {...props()} />);
    await act(async () => fireEvent.change(screen.getByLabelText("搜索主机树"), { target: { value: "a" } }));
    const restore = await dragOver(hostBtn("a3"), hostBtn("a1"), 105);
    try {
      expect(dropLines().length).toBe(0);
      await release();
    } finally {
      restore();
    }
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("搜索框里有字时分组不能拖", async () => {
    render(<VaultView {...props()} />);
    await act(async () => fireEvent.change(screen.getByLabelText("搜索主机树"), { target: { value: "a" } }));
    const restore = await dragOver(groupBtn(2), groupBtn(1), 105);
    try {
      expect(dropLines().length).toBe(0);
      expect(document.querySelector('[data-section="host-drag-ghost"]')).toBeNull();
      await release();
    } finally {
      restore();
    }
    expect(reorderGroups).not.toHaveBeenCalled();
  });

  it("搜索时换分组的拖动照旧：拖到分组行上还是移过去", async () => {
    render(<VaultView {...props()} />);
    await act(async () => fireEvent.change(screen.getByLabelText("搜索主机树"), { target: { value: "a" } }));
    const u1 = [...document.querySelectorAll('[data-section="tree-host"]')].find((el) =>
      el.textContent?.startsWith("u1"),
    ) as HTMLElement;
    const restore = await dragOver(u1, groupBtn(2), 110);
    try {
      await release();
    } finally {
      restore();
    }
    expect(saveHost).toHaveBeenCalledWith(expect.objectContaining({ id: 6, group_id: 2 }));
    expect(reorderHosts).not.toHaveBeenCalled();
  });

  it("没拖过门槛就是普通单击，不调整", async () => {
    // 上一个测试松手后的「忽略下一次单击」要等一个宏任务才解除
    await new Promise((resolve) => setTimeout(resolve, 0));
    render(<VaultView {...props()} />);
    const a1 = hostBtn("a1");
    fireEvent.pointerDown(a1, { button: 0, clientX: 10, clientY: 200 });
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 12, clientY: 201 }));
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 12, clientY: 201 }));
    fireEvent.click(a1);
    expect(reorderHosts).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="host-info"]')?.textContent).toContain("a1");
  });
});
