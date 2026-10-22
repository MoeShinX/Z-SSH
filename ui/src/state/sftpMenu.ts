//! SFTP 右键菜单的内容、禁用规则和位置（M3-5 第 1 节）。
//!
//! 菜单长什么样由一个纯函数按「哪一栏、点在文件 / 文件夹 / 空白处、选了几项、剪贴板里有什么」算出来，
//! 组件（`SftpContextMenu`）只负责画和键盘导航；每种情形都可以直接单元测试。

import type { SftpEntry } from "../api/sftp";

export type MenuSide = "local" | "remote";

/** 内置编辑器能打开的文件大小上限（和后端 `MAX_EDIT_BYTES` 一致：5 MB）。 */
export const EDIT_MAX_BYTES = 5 * 1024 * 1024;

/** 菜单项的动作名（`SftpTabView` 按它分派）。 */
export type MenuActionId =
  | "open"
  | "edit-builtin"
  | "transfer"
  | "download-to"
  | "copy"
  | "paste"
  | "copy-path"
  | "rename"
  | "delete"
  | "mkdir"
  | "create"
  | "permissions"
  | "refresh"
  | "properties";

export type MenuNode =
  | {
      kind: "item";
      id: MenuActionId;
      label: string;
      shortcut?: string;
      disabled: boolean;
      /** 禁用时悬停显示的原因。 */
      reason?: string;
    }
  | {
      kind: "submenu";
      id: string;
      label: string;
      disabled: boolean;
      reason?: string;
      children: MenuNode[];
    }
  | { kind: "separator" };

/** 剪贴板里有什么（菜单只关心这些）。 */
export interface MenuClipboard {
  /** 剪贴板里的东西来自哪一边：本地，或者某台主机的远端。 */
  side: MenuSide;
  count: number;
}

/** 哪些功能已经可用（分步实施时没做完的先禁用，写明原因）。 */
export interface MenuFeatures {
  builtinEditor: boolean;
  clipboard: boolean;
  properties: boolean;
}

export interface MenuContext {
  side: MenuSide;
  /** `items`：右键点在条目上（`entries` 是选中的）；`blank`：点在空白处。 */
  target: "items" | "blank";
  entries: SftpEntry[];
  /** 这一栏当前目录；本地的空字符串 = 「此电脑」（盘符列表），远端空字符串 = 还没进任何目录。 */
  currentDir: string;
  clipboard: MenuClipboard | null;
  features: MenuFeatures;
}

export const NOT_UTF8_REASON = "文件名编码不是 UTF-8，暂不支持操作";

const FEATURE_PENDING = "这个功能还在做";

function isDirLike(entry: SftpEntry): boolean {
  return (
    entry.kind === "dir" ||
    entry.kind === "drive" ||
    entry.target_kind === "dir"
  );
}

function item(
  id: MenuActionId,
  label: string,
  disabledReason: string | null,
  shortcut?: string,
): MenuNode {
  return {
    kind: "item",
    id,
    label,
    shortcut,
    disabled: disabledReason !== null,
    reason: disabledReason ?? undefined,
  };
}

const SEPARATOR: MenuNode = { kind: "separator" };

/** 去掉开头、结尾和连续的分隔线（某一组全被省略时不留空线）。 */
export function tidySeparators(nodes: MenuNode[]): MenuNode[] {
  const out: MenuNode[] = [];
  for (const node of nodes) {
    if (node.kind === "separator") {
      if (out.length === 0 || out[out.length - 1].kind === "separator")
        continue;
    }
    out.push(node);
  }
  while (out.length > 0 && out[out.length - 1].kind === "separator") out.pop();
  return out;
}

/** 为什么不能对这些条目做某类操作；`null` = 可以。 */
function selectionReason(
  entries: SftpEntry[],
  need: "one" | "some",
  options: { forbidDrives?: boolean } = {},
): string | null {
  if (entries.length === 0) return "先选中一个条目";
  if (need === "one" && entries.length !== 1) return "只能对单个条目操作";
  if (entries.some((entry) => !entry.name_is_utf8)) return NOT_UTF8_REASON;
  if (options.forbidDrives && entries.some((entry) => entry.kind === "drive")) {
    return "盘符不能这样操作";
  }
  return null;
}

/** 编辑类动作（内置编辑器）为什么不可用；`null` = 可以。 */
function editReason(entries: SftpEntry[]): string | null {
  if (entries.length === 0) return "先选中一个文件";
  if (entries.length !== 1) return "编辑只能对单个文件";
  const entry = entries[0];
  if (!entry.name_is_utf8) return NOT_UTF8_REASON;
  if (isDirLike(entry))
    return entry.kind === "drive" ? "盘符不能编辑" : "文件夹不能用编辑器打开";
  if (entry.kind === "other") return "这种特殊文件不能编辑";
  if (entry.size !== undefined && entry.size > EDIT_MAX_BYTES) {
    return `文件超过 ${EDIT_MAX_BYTES / 1024 / 1024} MB，请下载后再看`;
  }
  return null;
}

/** 粘贴为什么不可用；`null` = 可以。 */
export function pasteReason(
  context: Pick<MenuContext, "side" | "currentDir" | "clipboard" | "features">,
): string | null {
  if (!context.features.clipboard) return FEATURE_PENDING;
  const clip = context.clipboard;
  if (!clip || clip.count === 0) return "剪贴板是空的";
  if (clip.side !== context.side) return "远端和本地之间请用上传 / 下载";
  if (!context.currentDir) {
    return context.side === "local" ? "先进入一个文件夹" : "先进入一个目录";
  }
  return null;
}

/** 当前目录能不能新建东西 / 粘贴（远端没进任何目录、本地在「此电脑」时不行）。 */
function dirReason(
  context: Pick<MenuContext, "side" | "currentDir">,
): string | null {
  if (context.currentDir) return null;
  return context.side === "local" ? "先进入一个文件夹" : "先进入一个目录";
}

/** 按上下文算出完整菜单。 */
export function buildMenu(context: MenuContext): MenuNode[] {
  const { side, entries, features } = context;
  const blank = context.target === "blank";
  const isRemote = side === "remote";
  const newDirReason = dirReason(context);

  const newMenu: MenuNode = {
    kind: "submenu",
    id: "new",
    label: "新建",
    disabled: newDirReason !== null,
    reason: newDirReason ?? undefined,
    children: [
      item("mkdir", "文件夹", newDirReason),
      item("create", "文件", newDirReason),
    ],
  };
  const paste = item("paste", "粘贴", pasteReason(context), "Ctrl+V");
  const refresh = item("refresh", "刷新", null, "F5");
  const properties = item(
    "properties",
    "属性",
    !features.properties
      ? FEATURE_PENDING
      : blank
        ? newDirReason
        : selectionReason(entries, "some", { forbidDrives: true }),
    "Alt+Enter",
  );

  if (blank) {
    return tidySeparators([
      paste,
      SEPARATOR,
      newMenu,
      SEPARATOR,
      refresh,
      properties,
    ]);
  }

  const builtin = features.builtinEditor
    ? editReason(entries)
    : FEATURE_PENDING;
  const transferReason = selectionReason(entries, "some", {
    forbidDrives: true,
  });
  const copyReason = features.clipboard
    ? selectionReason(entries, "some", { forbidDrives: true })
    : FEATURE_PENDING;
  const openReason = selectionReason(entries, "one");

  const nodes: MenuNode[] = [
    item("open", "打开", openReason),
    item("edit-builtin", "用内置编辑器编辑", builtin),
    SEPARATOR,
    item("transfer", isRemote ? "下载到本地" : "上传到远端", transferReason),
    ...(isRemote ? [item("download-to", "下载到…", transferReason)] : []),
    SEPARATOR,
    item("copy", "复制", copyReason, "Ctrl+C"),
    paste,
    item(
      "copy-path",
      "复制路径",
      entries.length === 0 ? "先选中一个条目" : null,
    ),
    SEPARATOR,
    item(
      "rename",
      "重命名",
      selectionReason(entries, "one", { forbidDrives: true }),
      "F2",
    ),
    item(
      "delete",
      "删除",
      selectionReason(entries, "some", { forbidDrives: true }),
      "Del",
    ),
    SEPARATOR,
    newMenu,
    ...(isRemote
      ? [item("permissions", "修改权限", selectionReason(entries, "one"))]
      : []),
    SEPARATOR,
    refresh,
    properties,
  ];
  return tidySeparators(nodes);
}

/** 右键点在条目上时「粘贴」要粘贴到哪个目录：只选了一个文件夹就进这个文件夹，否则当前目录。 */
export function pasteTarget(entries: SftpEntry[], currentDir: string): string {
  if (
    entries.length === 1 &&
    entries[0].kind === "dir" &&
    entries[0].name_is_utf8
  ) {
    return entries[0].path;
  }
  return currentDir;
}

// ---------------------------------------------------------------------------
// 位置
// ---------------------------------------------------------------------------

export interface Size {
  width: number;
  height: number;
}

export interface MenuPlacement {
  left: number;
  top: number;
  /** 菜单比屏幕还高时限制高度、内部滚动。 */
  maxHeight?: number;
}

const EDGE = 8;

/**
 * 按菜单**实际**尺寸摆放：右边、下边放不下就往回挪（下边放不下且上面放得下时翻到点击点上方），
 * 永远不超出屏幕；比屏幕还高就限制高度。
 */
export function placeMenu(
  point: { x: number; y: number },
  menu: Size,
  viewport: Size,
): MenuPlacement {
  const availableHeight = Math.max(0, viewport.height - EDGE * 2);
  const maxHeight = menu.height > availableHeight ? availableHeight : undefined;
  const height = Math.min(menu.height, availableHeight);

  let left = point.x;
  if (left + menu.width > viewport.width - EDGE)
    left = viewport.width - EDGE - menu.width;
  left = Math.max(EDGE, left);

  let top = point.y;
  if (top + height > viewport.height - EDGE) {
    // 点击点上方放得下就翻上去，不然贴着下边。
    const above = point.y - height;
    top = above >= EDGE ? above : viewport.height - EDGE - height;
  }
  top = Math.max(EDGE, top);
  return { left, top, maxHeight };
}

/** 子菜单挨着父菜单项摆：右边放不下就放左边，上下同样不超出屏幕。 */
export function placeSubmenu(
  anchor: { left: number; top: number; right: number },
  menu: Size,
  viewport: Size,
): MenuPlacement {
  const availableHeight = Math.max(0, viewport.height - EDGE * 2);
  const height = Math.min(menu.height, availableHeight);
  const fitsRight = anchor.right + menu.width <= viewport.width - EDGE;
  let left = fitsRight ? anchor.right : anchor.left - menu.width;
  left = Math.max(EDGE, Math.min(left, viewport.width - EDGE - menu.width));
  let top = anchor.top;
  if (top + height > viewport.height - EDGE)
    top = viewport.height - EDGE - height;
  top = Math.max(EDGE, top);
  return {
    left,
    top,
    maxHeight: menu.height > availableHeight ? availableHeight : undefined,
  };
}

/** 方向键在可用项里移动（跳过禁用项和分隔线；到头回绕）。 */
export function nextEnabledIndex(
  nodes: MenuNode[],
  from: number,
  delta: 1 | -1,
): number {
  const count = nodes.length;
  if (count === 0) return -1;
  // 还没有选中项（-1）时：向下从第一项开始，向上从最后一项开始。
  let index = from < 0 && delta === -1 ? count : from;
  for (let step = 0; step < count; step += 1) {
    index = (index + delta + count) % count;
    const node = nodes[index];
    if (node.kind !== "separator" && !node.disabled) return index;
  }
  return -1;
}
