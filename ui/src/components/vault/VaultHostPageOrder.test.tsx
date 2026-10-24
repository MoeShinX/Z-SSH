/** @vitest-environment jsdom */
//! 主机页右边（UI-16）：排序方式（手动 / 最近打开 / 名称）、同层拖动排序、右键上移 / 下移。

import { act, cleanup, fireEvent, render } from "@testing-library/react";
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

import { settingsSet } from "../../api/settings";
import type { Group, Host } from "../../api/vault";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import { toast } from "../ui/toast";
import { resetModeHintForTests } from "./hostDrag";
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

// Alpha(1)：a1 a2 a3 + 子分组 Sub(3)：s1；Beta(2)：b1 b2；顶层：u1 u2
const treeGroups = [group(1, "Alpha"), group(2, "Beta"), group(3, "Sub", 1)];
const treeHosts = [
  host(1, "a1", 1),
  host(2, "a2", 1),
  host(3, "a3", 1),
  host(4, "b1", 2),
  host(5, "b2", 2),
  host(6, "u1", null),
  host(7, "u2", null),
  host(8, "s1", 3),
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
let saveHost: ReturnType<typeof vi.fn>;

function seed(groups: Group[], hosts: Host[]) {
  reorderHosts = vi.fn(async () => {});
  saveHost = vi.fn(async () => ({}) as Host);
  useVault.setState({ groups, hosts, load: async () => {}, reorderHosts, saveHost } as never);
}

function settings(extra: Record<string, unknown> = {}) {
  useSettings.setState({
    "layout.vault_expanded": "g1,g2,g3",
    "layout.vault_sidebar_collapsed": false,
    "layout.vault_view_mode": "grid",
    "layout.vault_density": "compact",
    "layout.vault_sort_mode": "manual",
    "layout.recent_hosts": "",
    vault_ungrouped_flat: false,
    ...extra,
  } as never);
}

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  resetModeHintForTests();
  vi.mocked(settingsSet).mockClear();
  seed(treeGroups, treeHosts);
  settings();
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const pageNames = () =>
  [...document.querySelectorAll('[data-section="host-card"], [data-section="host-row"]')].map(
    (el) => el.querySelector(".truncate")?.textContent,
  );
const idOf = (name: string) => treeHosts.find((entry) => entry.name === name)?.id as number;
const item = (name: string) => document.querySelector(`[data-page-host="${idOf(name)}"]`) as HTMLElement;
const dropLines = () => [...document.querySelectorAll('[data-section="drop-line"]')];

/** 按下 `from`，指针移到 `target` 的 (x, y)；target 的矩形：左 100 宽 200 / 上 100 高 50。拖动一直保持，直到 release。 */
async function dragOver(from: HTMLElement, target: HTMLElement, x: number, y: number) {
  const original = document.elementFromPoint;
  document.elementFromPoint = () => target;
  target.getBoundingClientRect = () =>
    ({ left: 100, width: 200, right: 300, top: 100, height: 50, bottom: 150 }) as DOMRect;
  fireEvent.pointerDown(from, { button: 0, clientX: 10, clientY: 300 });
  await act(async () => {
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: x, clientY: y }));
  });
  return () => {
    document.elementFromPoint = original;
  };
}
async function release() {
  await act(async () => {
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 0, clientY: 0 }));
  });
}
const search = (text: string) =>
  act(async () => {
    fireEvent.change(document.querySelector("header input") as HTMLElement, { target: { value: text } });
  });

describe("排序方式", () => {
  it("默认手动：右边的顺序和左边的树一致（子分组先、顶层主机在最后）", () => {
    expect(useSettings.getState()["layout.vault_sort_mode"]).toBe("manual");
    render(<VaultView {...props()} />);
    expect(pageNames()).toEqual(["s1", "a1", "a2", "a3", "b1", "b2", "u1", "u2"]);
    const treeOrder = [...document.querySelectorAll('[data-section="tree-host"]')].map((el) => el.textContent);
    expect(treeOrder).toEqual(["s1", "a1", "a2", "a3", "b1", "b2"]);
  });

  it("手动顺序调整过：右边跟着变；选了分组只显示它和子分组", () => {
    seed(treeGroups, [
      { ...host(1, "a1", 1), sort: 3 },
      { ...host(2, "a2", 1), sort: 1 },
      { ...host(3, "a3", 1), sort: 2 },
      ...treeHosts.slice(3),
    ]);
    render(<VaultView {...props()} />);
    expect(pageNames().slice(0, 4)).toEqual(["s1", "a2", "a3", "a1"]);
    act(() => fireEvent.click(document.querySelector('[data-order-group="1"]') as HTMLElement));
    expect(pageNames()).toEqual(["s1", "a2", "a3", "a1"]);
  });

  it("顶部的排序按钮按 手动 → 最近打开 → 名称 → 手动 轮换，并写进设置", async () => {
    settings({ "layout.recent_hosts": "7,2" });
    render(<VaultView {...props()} />);
    const button = document.querySelector('[data-action="cycle-sort"]') as HTMLElement;
    expect(button.textContent).toBe("手动");
    await act(async () => fireEvent.click(button));
    expect(useSettings.getState()["layout.vault_sort_mode"]).toBe("recent");
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_sort_mode", JSON.stringify("recent"));
    expect(button.textContent).toBe("最近打开");
    expect(pageNames().slice(0, 3)).toEqual(["u2", "a2", "a1"]);
    await act(async () => fireEvent.click(button));
    expect(button.textContent).toBe("名称");
    expect(pageNames()).toEqual(["a1", "a2", "a3", "b1", "b2", "s1", "u1", "u2"]);
    await act(async () => fireEvent.click(button));
    expect(useSettings.getState()["layout.vault_sort_mode"]).toBe("manual");
    expect(pageNames()[0]).toBe("s1");
  });

  it("排序方式只影响右边：左边的树不变", () => {
    settings({ "layout.vault_sort_mode": "name" });
    render(<VaultView {...props()} />);
    const treeNames = [...document.querySelectorAll('[data-section="tree-host"]')].map((el) => el.textContent);
    expect(treeNames.slice(0, 2)).toEqual(["s1", "a1"]);
  });
});

for (const layout of ["grid", "list"] as const) {
  describe(`右边拖动排序（${layout === "grid" ? "网格" : "列表"}）`, () => {
    // 网格看左右半（x），列表看上下半（y）
    const before = (layout === "grid" ? [120, 125] : [200, 110]) as [number, number];
    const after = (layout === "grid" ? [280, 125] : [200, 140]) as [number, number];

    beforeEach(() => settings({ "layout.vault_view_mode": layout }));

    it("同一分组拖到另一台的前半：画插入线（前面），松手整层按新顺序重新编号", async () => {
      render(<VaultView {...props()} />);
      const target = item("a1");
      const restore = await dragOver(item("a3"), target, ...before);
      try {
        expect(dropLines().length).toBe(1);
        const line = target.querySelector('[data-section="drop-line"]') as HTMLElement;
        expect(line.getAttribute("data-position")).toBe("before");
        expect(line.getAttribute("data-orientation")).toBe(layout === "grid" ? "vertical" : null);
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).toHaveBeenCalledWith([3, 1, 2]);
      expect(saveHost).not.toHaveBeenCalled();
      expect(dropLines().length).toBe(0);
    });

    it("拖动时整张卡片的影子跟着指针走，原来那张变淡；松手后都还原", async () => {
      render(<VaultView {...props()} />);
      const source = item("a3");
      const sourceName = source.querySelector(".truncate")?.textContent;
      const restore = await dragOver(source, item("a1"), ...before);
      try {
        const card = document.querySelector('[data-section="host-drag-card"]') as HTMLElement;
        expect(card).not.toBeNull();
        // 影子是卡片的样子（有主机名），而且不带 data-* 属性，不会和真的卡片混淆
        expect(card.textContent).toContain(sourceName);
        expect(card.querySelector("[data-page-host], [data-section=host-card], [data-section=host-row]")).toBeNull();
        expect(source.style.opacity).toBe("0.35");
        // 小标签还在，挂在影子下面
        expect(document.querySelector('[data-section="host-drag-ghost"]')).not.toBeNull();
        await release();
      } finally {
        restore();
      }
      expect(document.querySelector('[data-section="host-drag-card"]')).toBeNull();
      expect(source.style.opacity).toBe("");
    });

    it("后半：插到它后面", async () => {
      render(<VaultView {...props()} />);
      const target = item("a1");
      const restore = await dragOver(item("a3"), target, ...after);
      try {
        expect(target.querySelector('[data-section="drop-line"]')?.getAttribute("data-position")).toBe("after");
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).toHaveBeenCalledWith([1, 3, 2]);
    });

    it("左边的树里同一台主机的行不跟着画线", async () => {
      render(<VaultView {...props()} />);
      const restore = await dragOver(item("a3"), item("a1"), ...before);
      try {
        expect(document.querySelectorAll('[data-section="tree-host"] [data-section="drop-line"]').length).toBe(0);
        await release();
      } finally {
        restore();
      }
    });

    it("位置没变什么都不做", async () => {
      render(<VaultView {...props()} />);
      const restore = await dragOver(item("a1"), item("a2"), ...before);
      try {
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).not.toHaveBeenCalled();
    });

    it("拖到不同分组的主机上：有插入线、提示写明去向，松手先换分组再按位置排", async () => {
      render(<VaultView {...props()} />);
      const target = item("b1");
      const restore = await dragOver(item("a1"), target, ...before);
      try {
        expect(target.querySelector('[data-section="drop-line"]')?.getAttribute("data-position")).toBe("before");
        const label = document.querySelector('[data-section="host-drag-ghost"]')?.textContent ?? "";
        expect(label).toContain("→");
        await release();
      } finally {
        restore();
      }
      // 先换到目标所在的分组，再把目标那一层按新位置整层重新编号
      expect(saveHost).toHaveBeenCalledTimes(1);
      expect(vi.mocked(saveHost).mock.calls[0][0]).toMatchObject({ id: idOf("a1"), group_id: treeHosts.find((h) => h.name === "b1")?.group_id });
      expect(reorderHosts).toHaveBeenCalledTimes(1);
      const ids = vi.mocked(reorderHosts).mock.calls[0][0] as number[];
      expect(ids.indexOf(idOf("a1"))).toBe(ids.indexOf(idOf("b1")) - 1);
    });

    it("顶层主机之间也能排序", async () => {
      render(<VaultView {...props()} />);
      const restore = await dragOver(item("u2"), item("u1"), ...before);
      try {
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).toHaveBeenCalledWith([7, 6]);
    });

    it("搜索中不响应：没有插入线，松手不调整，也不弹提示", async () => {
      const info = vi.spyOn(toast, "info").mockImplementation(() => {});
      render(<VaultView {...props()} />);
      await search("a");
      const restore = await dragOver(item("a3"), item("a1"), ...before);
      try {
        expect(dropLines().length).toBe(0);
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).not.toHaveBeenCalled();
      expect(info).not.toHaveBeenCalled();
      info.mockRestore();
    });

    it("不是手动排序：不响应，松手时提示一次「切换到「手动」排序后才能拖动」", async () => {
      settings({ "layout.vault_sort_mode": "name", "layout.vault_view_mode": layout });
      const info = vi.spyOn(toast, "info").mockImplementation(() => {});
      render(<VaultView {...props()} />);
      for (let round = 0; round < 2; round += 1) {
        await new Promise((resolve) => setTimeout(resolve, 0));
        const restore = await dragOver(item("a3"), item("a1"), ...before);
        try {
          expect(dropLines().length).toBe(0);
          await release();
        } finally {
          restore();
        }
      }
      expect(reorderHosts).not.toHaveBeenCalled();
      expect(info).toHaveBeenCalledTimes(1);
      expect(info).toHaveBeenCalledWith("切换到「手动」排序后才能拖动");
      info.mockRestore();
    });

    it("拖动后紧跟的那次 click 不算单击选中；没拖过门槛仍然是单击，双击照常连接", async () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      const source = item("a3");
      const restore = await dragOver(source, item("a1"), ...before);
      try {
        await act(async () => {
          window.dispatchEvent(new MouseEvent("pointerup", { clientX: 0, clientY: 0 }));
          fireEvent.click(source);
        });
      } finally {
        restore();
      }
      expect(reorderHosts).toHaveBeenCalledTimes(1);
      expect(source.getAttribute("data-selected")).toBeNull();
      expect(handlers.onOpenHost).not.toHaveBeenCalled();

      await new Promise((resolve) => setTimeout(resolve, 0));
      const other = item("a2");
      fireEvent.pointerDown(other, { button: 0, clientX: 10, clientY: 200 });
      window.dispatchEvent(new MouseEvent("pointermove", { clientX: 12, clientY: 201 }));
      window.dispatchEvent(new MouseEvent("pointerup", { clientX: 12, clientY: 201 }));
      fireEvent.click(other);
      expect(other.getAttribute("data-selected")).toBe("true");
      fireEvent.doubleClick(other);
      expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }));
      expect(reorderHosts).toHaveBeenCalledTimes(1);
    });

    it("卡片里的按钮不是拖动的把手", async () => {
      render(<VaultView {...props()} />);
      const edit = item("a3").querySelector('[data-action="edit-host"]') as HTMLElement;
      const restore = await dragOver(edit, item("a1"), ...before);
      try {
        expect(dropLines().length).toBe(0);
        await release();
      } finally {
        restore();
      }
      expect(reorderHosts).not.toHaveBeenCalled();
    });
  });
}

describe("右键上移 / 下移（右边）", () => {
  const action = (name: "up" | "down") => document.querySelector(`[data-order-action="${name}"]`) as HTMLElement;
  const disabled = (el: HTMLElement) => el.getAttribute("aria-disabled") === "true";
  const openMenu = (el: HTMLElement) => act(async () => fireEvent.contextMenu(el));

  it("手动排序：上移传整层的新顺序；一层里第一个不能上移、最后一个不能下移", async () => {
    render(<VaultView {...props()} />);
    await openMenu(item("a2"));
    expect(disabled(action("up"))).toBe(false);
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenCalledWith([2, 1, 3]);
    cleanup();

    render(<VaultView {...props()} />);
    await openMenu(item("a1"));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(false);
    cleanup();

    render(<VaultView {...props()} />);
    await openMenu(item("a3"));
    expect(disabled(action("down"))).toBe(true);
    await act(async () => action("up").click());
    expect(reorderHosts).toHaveBeenLastCalledWith([1, 3, 2]);
  });

  it("下移", async () => {
    render(<VaultView {...props()} />);
    await openMenu(item("b1"));
    await act(async () => action("down").click());
    expect(reorderHosts).toHaveBeenCalledWith([5, 4]);
  });

  it("最近打开 / 名称排序时置灰，悬停提示「切到手动排序才能调整」", async () => {
    for (const mode of ["recent", "name"]) {
      settings({ "layout.vault_sort_mode": mode });
      render(<VaultView {...props()} />);
      await openMenu(item("a2"));
      expect(disabled(action("up"))).toBe(true);
      expect(disabled(action("down"))).toBe(true);
      expect(action("up").closest("[data-section='order-items']")?.getAttribute("title")).toBe("切到手动排序才能调整");
      cleanup();
    }
  });

  it("搜索中置灰", async () => {
    render(<VaultView {...props()} />);
    await search("a2");
    await openMenu(item("a2"));
    expect(disabled(action("up"))).toBe(true);
    expect(disabled(action("down"))).toBe(true);
  });

  it("手动排序没在搜索时菜单项没有置灰提示", async () => {
    render(<VaultView {...props()} />);
    await openMenu(item("a2"));
    expect(action("up").closest("[data-section='order-items']")?.hasAttribute("title")).toBe(false);
  });
});
