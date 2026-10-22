import { describe, expect, it } from "vitest";

import type { SftpEntry, SftpTransfer } from "../api/sftp";
import {
  buildMenu,
  buildTransferMenu,
  EDIT_MAX_BYTES,
  nextEnabledIndex,
  NOT_UTF8_REASON,
  pasteReason,
  pasteTarget,
  placeMenu,
  placeSubmenu,
  tidySeparators,
  type MenuActionId,
  type MenuContext,
  type MenuNode,
} from "./sftpMenu";

const ALL_ON = { builtinEditor: true, clipboard: true, properties: true };

function entry(overrides: Partial<SftpEntry> & { name: string }): SftpEntry {
  return {
    path: `/srv/${overrides.name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    size: 100,
    ...overrides,
  };
}

const file = entry({ name: "a.conf" });
const folder = entry({ name: "logs", kind: "dir" });

function context(overrides: Partial<MenuContext> = {}): MenuContext {
  return {
    side: "remote",
    target: "items",
    entries: [file],
    currentDir: "/srv",
    clipboard: null,
    features: ALL_ON,
    ...overrides,
  };
}

/** 摊平成 id → 节点（含子菜单里的）。 */
function flatten(nodes: MenuNode[]): Map<string, MenuNode> {
  const out = new Map<string, MenuNode>();
  for (const node of nodes) {
    if (node.kind === "separator") continue;
    out.set(node.id, node);
    if (node.kind === "submenu") {
      for (const [id, child] of flatten(node.children)) out.set(id, child);
    }
  }
  return out;
}

function enabled(nodes: MenuNode[], id: string): boolean {
  const node = flatten(nodes).get(id);
  if (!node || node.kind === "separator") throw new Error(`菜单里没有 ${id}`);
  return !node.disabled;
}

function reason(nodes: MenuNode[], id: string): string | undefined {
  const node = flatten(nodes).get(id);
  return node && node.kind !== "separator" ? node.reason : undefined;
}

/** 菜单的顺序：每项写 id，分隔线写 "-"，子菜单写 `id[子项…]`。 */
function shape(nodes: MenuNode[]): string {
  return nodes
    .map((node) =>
      node.kind === "separator"
        ? "-"
        : node.kind === "submenu"
          ? `${node.id}[${node.children.map((child) => (child.kind === "separator" ? "-" : child.id)).join(",")}]`
          : node.id,
    )
    .join(" ");
}

describe("远端菜单的结构（卡片 1 节的菜单）", () => {
  it("点在文件上：按卡片的顺序和分组", () => {
    const menu = buildMenu(context());
    expect(shape(menu)).toBe(
      "open edit-builtin - transfer download-to - copy paste copy-path - rename delete - new[mkdir,create] permissions - refresh properties",
    );
    const labels = menu.map((node) =>
      node.kind === "separator" ? "-" : node.label,
    );
    expect(labels).toEqual([
      "打开",
      "用内置编辑器编辑",
      "-",
      "下载到本地",
      "下载到…",
      "-",
      "复制",
      "粘贴",
      "复制路径",
      "-",
      "重命名",
      "删除",
      "-",
      "新建",
      "修改权限",
      "-",
      "刷新",
      "属性",
    ]);
  });

  it("快捷键标在菜单项上", () => {
    const flat = flatten(buildMenu(context()));
    const shortcut = (id: string) => {
      const node = flat.get(id);
      return node && node.kind === "item" ? node.shortcut : undefined;
    };
    expect(shortcut("copy")).toBe("Ctrl+C");
    expect(shortcut("paste")).toBe("Ctrl+V");
    expect(shortcut("rename")).toBe("F2");
    expect(shortcut("delete")).toBe("Del");
    expect(shortcut("refresh")).toBe("F5");
    expect(shortcut("properties")).toBe("Alt+Enter");
  });

  it("点在文件上，单选：除了粘贴（剪贴板空）都可用", () => {
    const menu = buildMenu(context());
    const ids: MenuActionId[] = [
      "open",
      "edit-builtin",
      "transfer",
      "download-to",
      "copy",
      "copy-path",
      "rename",
      "delete",
      "permissions",
      "refresh",
      "properties",
    ];
    for (const id of ids) expect(enabled(menu, id), id).toBe(true);
    expect(enabled(menu, "paste")).toBe(false);
    expect(reason(menu, "paste")).toBe("剪贴板是空的");
  });

  it("点在文件夹上：不能用编辑器打开，其余照常", () => {
    const menu = buildMenu(context({ entries: [folder] }));
    expect(enabled(menu, "open")).toBe(true);
    expect(enabled(menu, "edit-builtin")).toBe(false);
    expect(reason(menu, "edit-builtin")).toBe("文件夹不能用编辑器打开");
    expect(enabled(menu, "download-to")).toBe(true);
    expect(enabled(menu, "rename")).toBe(true);
    expect(enabled(menu, "permissions")).toBe(true);
  });

  it("多选：编辑、打开、重命名、修改权限只对单个条目；传输、复制、删除、复制路径可以", () => {
    const menu = buildMenu(
      context({ entries: [file, folder, entry({ name: "b" })] }),
    );
    for (const id of [
      "open",
      "edit-builtin",
      "rename",
      "permissions",
    ]) {
      expect(enabled(menu, id), id).toBe(false);
    }
    expect(reason(menu, "rename")).toBe("只能对单个条目操作");
    expect(reason(menu, "edit-builtin")).toBe("编辑只能对单个文件");
    for (const id of [
      "transfer",
      "download-to",
      "copy",
      "delete",
      "copy-path",
    ]) {
      expect(enabled(menu, id), id).toBe(true);
    }
  });

  it("名字不是 UTF-8 的条目：除了刷新、属性、新建，什么操作都不行，原因写明", () => {
    const bad = entry({ name: "\uFFFD.txt", name_is_utf8: false });
    const menu = buildMenu(context({ entries: [bad] }));
    for (const id of [
      "open",
      "edit-builtin",
      "transfer",
      "download-to",
      "copy",
      "rename",
      "delete",
      "permissions",
    ]) {
      expect(enabled(menu, id), id).toBe(false);
      expect(reason(menu, id), id).toBe(NOT_UTF8_REASON);
    }
    expect(enabled(menu, "refresh")).toBe(true);
    expect(enabled(menu, "mkdir")).toBe(true);
    // 选中里混进一个不能操作的，整组都不行。
    const mixed = buildMenu(context({ entries: [file, bad] }));
    expect(enabled(mixed, "delete")).toBe(false);
    expect(enabled(mixed, "copy")).toBe(false);
  });

  it("超过大小上限的文件不能用编辑器打开，悬停说明原因", () => {
    const big = entry({ name: "dump.sql", size: EDIT_MAX_BYTES + 1 });
    const menu = buildMenu(context({ entries: [big] }));
    expect(enabled(menu, "edit-builtin")).toBe(false);
    expect(reason(menu, "edit-builtin")).toContain("超过 5 MB");
    const exact = entry({ name: "ok.txt", size: EDIT_MAX_BYTES });
    expect(
      enabled(buildMenu(context({ entries: [exact] })), "edit-builtin"),
    ).toBe(true);
  });

  it("符号链接：指向文件可以编辑，指向文件夹不行", () => {
    const toFile = entry({ name: "l1", kind: "symlink", target_kind: "file" });
    const toDir = entry({ name: "l2", kind: "symlink", target_kind: "dir" });
    expect(
      enabled(buildMenu(context({ entries: [toFile] })), "edit-builtin"),
    ).toBe(true);
    expect(
      enabled(buildMenu(context({ entries: [toDir] })), "edit-builtin"),
    ).toBe(false);
  });

  it("进不了任何目录（路径为空）时不能新建，也不能粘贴", () => {
    const menu = buildMenu(
      context({
        currentDir: "",
        clipboard: { side: "remote", count: 1 },
      }),
    );
    expect(enabled(menu, "new")).toBe(false);
    expect(enabled(menu, "mkdir")).toBe(false);
    expect(enabled(menu, "paste")).toBe(false);
    expect(reason(menu, "new")).toBe("先进入一个目录");
  });
});

describe("本地菜单", () => {
  const local = (overrides: Partial<MenuContext> = {}) =>
    context({ side: "local", currentDir: "C:\\work", ...overrides });

  it("传输换成「上传到远端」，没有「下载到…」和「修改权限」", () => {
    const menu = buildMenu(local());
    expect(shape(menu)).toBe(
      "open edit-builtin - transfer - copy paste copy-path - rename delete - new[mkdir,create] - refresh properties",
    );
    const transfer = flatten(menu).get("transfer");
    expect(transfer && transfer.kind === "item" && transfer.label).toBe(
      "上传到远端",
    );
  });

  it("盘符（此电脑）：不能复制、重命名、删除、上传、编辑", () => {
    const drive = entry({ name: "C:", kind: "drive", path: "C:\\" });
    const menu = buildMenu(local({ entries: [drive], currentDir: "" }));
    for (const id of [
      "edit-builtin",
      "transfer",
      "copy",
      "rename",
      "delete",
    ]) {
      expect(enabled(menu, id), id).toBe(false);
    }
    expect(enabled(menu, "open")).toBe(true);
    expect(enabled(menu, "new")).toBe(false);
    expect(reason(menu, "new")).toBe("先进入一个文件夹");
  });
});

describe("空白处", () => {
  it("远端空白处只有 粘贴、新建、刷新、属性", () => {
    const menu = buildMenu(context({ target: "blank", entries: [] }));
    expect(shape(menu)).toBe("paste - new[mkdir,create] - refresh properties");
  });

  it("本地空白处同样", () => {
    const menu = buildMenu(
      context({
        side: "local",
        target: "blank",
        entries: [],
        currentDir: "C:\\work",
      }),
    );
    expect(shape(menu)).toBe("paste - new[mkdir,create] - refresh properties");
  });

  it("有可粘贴的东西时粘贴可用", () => {
    const menu = buildMenu(
      context({
        target: "blank",
        entries: [],
        clipboard: { side: "remote", count: 2 },
      }),
    );
    expect(enabled(menu, "paste")).toBe(true);
  });
});

describe("粘贴的可用性", () => {
  const base = {
    side: "remote" as const,
    currentDir: "/srv",
    features: ALL_ON,
  };

  it("剪贴板为空", () => {
    expect(pasteReason({ ...base, clipboard: null })).toBe("剪贴板是空的");
    expect(
      pasteReason({
        ...base,
        clipboard: { side: "remote", count: 0 },
      }),
    ).toBe("剪贴板是空的");
  });

  it("本地和远端之间不能粘贴，提示用上传 / 下载", () => {
    const clip = { side: "local" as const, count: 1 };
    expect(pasteReason({ ...base, clipboard: clip })).toBe(
      "远端和本地之间请用上传 / 下载",
    );
    expect(
      pasteReason({
        ...base,
        side: "local",
        currentDir: "C:\\x",
        clipboard: { side: "remote", count: 1 },
      }),
    ).toBe("远端和本地之间请用上传 / 下载");
  });

  it("同一边可以粘贴，远端到远端（含另一台主机）和本地到本地都行", () => {
    expect(
      pasteReason({
        ...base,
        clipboard: { side: "remote", count: 1 },
      }),
    ).toBeNull();
    expect(
      pasteReason({
        side: "local",
        currentDir: "C:\\x",
        features: ALL_ON,
        clipboard: { side: "local", count: 3 },
      }),
    ).toBeNull();
  });

  it("功能还没做完时禁用并说明", () => {
    expect(
      pasteReason({
        ...base,
        features: { ...ALL_ON, clipboard: false },
        clipboard: { side: "remote", count: 1 },
      }),
    ).toBe("这个功能还在做");
  });
});

describe("粘贴的目标目录", () => {
  it("只选了一个文件夹：粘贴进这个文件夹；其他情况粘贴到当前目录", () => {
    expect(pasteTarget([folder], "/srv")).toBe("/srv/logs");
    expect(pasteTarget([file], "/srv")).toBe("/srv");
    expect(pasteTarget([folder, file], "/srv")).toBe("/srv");
    expect(pasteTarget([], "/srv")).toBe("/srv");
    expect(
      pasteTarget(
        [entry({ name: "x", kind: "dir", name_is_utf8: false })],
        "/srv",
      ),
    ).toBe("/srv");
  });
});

describe("功能还没做完时", () => {
  it("内置编辑器、复制 / 粘贴、属性先禁用，写明原因，其他不受影响", () => {
    const menu = buildMenu(
      context({
        features: { builtinEditor: false, clipboard: false, properties: false },
      }),
    );
    for (const id of ["edit-builtin", "copy", "paste", "properties"]) {
      expect(enabled(menu, id), id).toBe(false);
      expect(reason(menu, id), id).toBe("这个功能还在做");
    }
    expect(enabled(menu, "rename")).toBe(true);
  });
});

describe("分隔线", () => {
  it("不留开头、结尾和连续的分隔线", () => {
    const sep: MenuNode = { kind: "separator" };
    const a: MenuNode = {
      kind: "item",
      id: "open",
      label: "a",
      disabled: false,
    };
    expect(tidySeparators([sep, a, sep, sep, a, sep])).toEqual([a, sep, a]);
    expect(tidySeparators([sep, sep])).toEqual([]);
  });
});

describe("菜单位置", () => {
  const viewport = { width: 1000, height: 700 };
  const menu = { width: 220, height: 480 };

  it("空间够就摆在点击点", () => {
    expect(placeMenu({ x: 100, y: 50 }, menu, viewport)).toEqual({
      left: 100,
      top: 50,
      maxHeight: undefined,
    });
  });

  it("靠近右边：往左挪，右边缘不超出屏幕", () => {
    const placed = placeMenu({ x: 990, y: 50 }, menu, viewport);
    expect(placed.left + menu.width).toBeLessThanOrEqual(viewport.width);
    expect(placed.left).toBeLessThan(990);
  });

  it("靠近底部：上面放得下就翻到点击点上方", () => {
    const placed = placeMenu({ x: 100, y: 650 }, menu, viewport);
    expect(placed.top + menu.height).toBeLessThanOrEqual(650);
    expect(placed.top).toBeGreaterThanOrEqual(0);
  });

  it("上下都放不下：贴着下边，不超出", () => {
    const placed = placeMenu(
      { x: 100, y: 300 },
      { width: 220, height: 650 },
      viewport,
    );
    expect(placed.top).toBeGreaterThanOrEqual(0);
    expect(placed.top + 650).toBeLessThanOrEqual(viewport.height);
  });

  it("比屏幕还高：限制高度，菜单内部滚动", () => {
    const placed = placeMenu(
      { x: 100, y: 300 },
      { width: 220, height: 900 },
      viewport,
    );
    expect(placed.maxHeight).toBeDefined();
    expect(placed.maxHeight!).toBeLessThanOrEqual(viewport.height);
    expect(placed.top).toBeGreaterThanOrEqual(0);
    expect(placed.top + placed.maxHeight!).toBeLessThanOrEqual(viewport.height);
  });

  it("屏幕上任何一个点击点，菜单都完整落在屏幕里", () => {
    for (let x = 0; x <= viewport.width; x += 37) {
      for (let y = 0; y <= viewport.height; y += 29) {
        const placed = placeMenu({ x, y }, menu, viewport);
        const height = placed.maxHeight ?? menu.height;
        expect(placed.left, `x=${x} y=${y}`).toBeGreaterThanOrEqual(0);
        expect(placed.top, `x=${x} y=${y}`).toBeGreaterThanOrEqual(0);
        expect(placed.left + menu.width, `x=${x} y=${y}`).toBeLessThanOrEqual(
          viewport.width,
        );
        expect(placed.top + height, `x=${x} y=${y}`).toBeLessThanOrEqual(
          viewport.height,
        );
      }
    }
  });

  it("很小的窗口也不超出", () => {
    const tiny = { width: 260, height: 300 };
    for (let x = 0; x <= tiny.width; x += 20) {
      for (let y = 0; y <= tiny.height; y += 20) {
        const placed = placeMenu({ x, y }, menu, tiny);
        const height = placed.maxHeight ?? menu.height;
        expect(placed.left).toBeGreaterThanOrEqual(0);
        expect(placed.top).toBeGreaterThanOrEqual(0);
        expect(placed.left + menu.width).toBeLessThanOrEqual(tiny.width);
        expect(placed.top + height).toBeLessThanOrEqual(tiny.height);
      }
    }
  });

  it("子菜单：右边放得下就摆在父菜单项右边，放不下摆左边，不超出屏幕", () => {
    const sub = { width: 160, height: 90 };
    const right = placeSubmenu(
      { left: 100, top: 200, right: 320 },
      sub,
      viewport,
    );
    expect(right.left).toBe(320);
    const left = placeSubmenu(
      { left: 800, top: 200, right: 980 },
      sub,
      viewport,
    );
    expect(left.left).toBe(800 - 160);
    const bottom = placeSubmenu(
      { left: 100, top: 690, right: 320 },
      sub,
      viewport,
    );
    expect(bottom.top + sub.height).toBeLessThanOrEqual(viewport.height);
  });
});

describe("键盘移动", () => {
  const nodes = buildMenu(context({ entries: [folder] }));

  it("跳过禁用项和分隔线，到头回绕", () => {
    let index = -1;
    const visited: string[] = [];
    for (let step = 0; step < nodes.length + 2; step += 1) {
      index = nextEnabledIndex(nodes, index, 1);
      const node = nodes[index];
      expect(node.kind).not.toBe("separator");
      if (node.kind !== "separator") {
        expect(node.disabled).toBe(false);
        visited.push(node.id);
      }
    }
    expect(visited[0]).toBe("open");
    // 禁用的编辑项被跳过。
    expect(visited).not.toContain("edit-builtin");
    expect(visited).not.toContain("paste");
    // 回绕：走完一圈又回到第一项。
    expect(new Set(visited).size).toBeLessThan(visited.length);
  });

  it("向上从头回绕到最后一个可用项", () => {
    const last = nextEnabledIndex(nodes, 0, -1);
    const node = nodes[last];
    expect(node.kind !== "separator" && node.id).toBe("properties");
  });

  it("还没有选中项时：向下是第一个可用项，向上是最后一个可用项", () => {
    const first = nextEnabledIndex(nodes, -1, 1);
    const last = nextEnabledIndex(nodes, -1, -1);
    expect(nodes[first].kind !== "separator" && nodes[first].id).toBe("open");
    expect(nodes[last].kind !== "separator" && nodes[last].id).toBe(
      "properties",
    );
  });

  it("全是禁用项时返回 -1", () => {
    const none = buildMenu(
      context({
        entries: [],
        features: { builtinEditor: false, clipboard: false, properties: false },
      }),
    ).filter((node) => node.kind === "separator" || node.disabled);
    expect(nextEnabledIndex(none, -1, 1)).toBe(-1);
  });
});

describe("传输队列一行的右键菜单", () => {
  const labels = (state: SftpTransfer["state"]) =>
    buildTransferMenu(state).map((node) => (node.kind === "item" ? node.label : node.kind));

  it("按状态显示，和右边图标按钮出现的条件一一对应", () => {
    expect(labels("queued")).toEqual(["暂停", "取消"]);
    expect(labels("planning")).toEqual(["暂停", "取消"]);
    expect(labels("running")).toEqual(["暂停", "取消"]);
    expect(labels("paused")).toEqual(["继续", "取消"]);
    expect(labels("waiting_reconnect")).toEqual(["取消"]);
    // 等同名文件的回答时暂停没有意义，只能取消
    expect(labels("waiting_conflict")).toEqual(["取消"]);
    expect(labels("failed")).toEqual(["重试"]);
    expect(labels("cancelled")).toEqual(["重试"]);
    // 已跳过也能重试（重新问一次同名文件怎么处理）
    expect(labels("skipped")).toEqual(["重试"]);
    // 已完成什么都没有：不弹菜单
    expect(labels("done")).toEqual([]);
  });

  it("每一项的动作名对得上，且都可用", () => {
    const ids = (state: SftpTransfer["state"]) =>
      buildTransferMenu(state).map((node) => (node.kind === "item" ? node.id : null));
    expect(ids("skipped")).toEqual(["transfer-retry"]);
    expect(ids("paused")).toEqual(["transfer-resume", "transfer-cancel"]);
    expect(ids("running")).toEqual(["transfer-pause", "transfer-cancel"]);
    for (const node of buildTransferMenu("failed")) {
      expect(node.kind === "item" && node.disabled).toBe(false);
    }
  });
});
