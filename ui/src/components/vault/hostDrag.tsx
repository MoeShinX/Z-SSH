//! 主机树里的拖放：拖主机换分组、拖主机 / 分组调整上下顺序（UI-9）。
//!
//! 用指针事件自己做：Tauri 默认开着 dragDropEnabled，Windows 上 WebView 里的 HTML5 拖放用不了
//! （标签栏拖 SFTP 也是这么做的）。
//!
//! - 换分组：放的目标是带 `data-drop-group` 的行：分组行是那个分组，"未分组"行是 `none`，
//!   树里的主机行是它所在的分组（拖到同组的主机上等于放进那个组，什么都不变）。
//! - 调整顺序：树里的主机行带 `data-order-host`、分组行带 `data-order-group` /
//!   `data-order-parent`。指针在主机行上：上半部分 = 插到它前面、下半部分 = 插到它后面，画一条
//!   插入线，松手就调整（目标在别的分组里就同时换过去）；分组只能在同一个父分组下调整。
//! - 平铺模式（UI-14，「未分组」改过名）：树里没有「未分组」行，滚动区域带 `data-drop-blank="none"`：
//!   拖到树的空白处（不在任何一行上）= 移到顶层；拖到根层主机之间 = 移到顶层并插在那个位置。
//!   树的根元素带 `data-order-locked="true"`（搜索框里有字）时不响应排序拖动，换分组照旧。
//! - 主机页右边的卡片 / 列表（UI-16）：每张卡片 / 行带 `data-page-host`（主机 id）和 `data-page-group`
//!   （所在分组），容器带 `data-page-layout`（grid 看左右半、list 看上下半）和 `data-page-order`：
//!   `manual` 才能排序，`mode`（不是手动排序）/ `search`（搜索中）不响应。拖到同一分组的主机前 / 后 =
//!   调整顺序；拖到**别的分组**的主机前 / 后 = 移到那个分组并插在那个位置（和左边的树一致，提示标签
//!   写明去向）。松手都走 `dropHostAt`（整层重新编号）。

import { Folder, Server } from "lucide-react";
import { create } from "zustand";

import { draftOfHost, type Group, type Host } from "../../api/vault";
import { cn } from "../../lib/utils";
import { insertInLayer } from "../../state/hostTree";
import { isUngroupedFlat, ungroupedLabel, useUngroupedFlat } from "../../state/ungroupedFlat";
import { useVault } from "../../stores/vault";
import { toast } from "../ui/toast";
import { errorText, groupLayerIds, hostLayerIds } from "./treeOrder";

const DRAG_THRESHOLD = 6;

/** 放到哪：分组 id，或者未分组。 */
export type DropGroup = number | null;

/** 插入线的位置：插在这一行的前面还是后面。 */
export interface InsertMark {
  kind: "host" | "group";
  id: number;
  after: boolean;
  /** 画在主机页右边的卡片 / 行上（不是左边的树）；树里同一台主机的行不跟着画线。 */
  scope?: "page";
}

/** 拖右边的卡片 / 行时跟着指针走的「影子」：把整张卡片的样子复制一份。 */
interface DragGhostInfo {
  /** 卡片的 HTML（已去掉 data-* 等属性，免得和真的卡片混淆）。 */
  html: string;
  width: number;
  height: number;
  /** 按下的位置相对卡片左上角的偏移，影子按同样的位置抓在指针下。 */
  offsetX: number;
  offsetY: number;
}

interface DragState {
  kind: "host" | "group";
  id: number;
  label: string;
  x: number;
  y: number;
  /** 指针下面的换分组目标；undefined = 不在任何目标上 */
  over: DropGroup | undefined;
  /** 指针下面的排序插入位置；null = 不在排序目标上 */
  insert: InsertMark | null;
  /** 拖的是右边的卡片 / 行时才有：跟着指针走的卡片影子。 */
  ghost?: DragGhostInfo | null;
}

export const useHostDrag = create<{ drag: DragState | null }>(() => ({ drag: null }));

let suppressClick = false;

/** 拖完松手时会接着触发一次 click（会选中主机）：单击处理里先问一下要不要忽略。 */
export function dragJustEnded(): boolean {
  return suppressClick;
}

function parseTarget(value: string | null | undefined): DropGroup | undefined {
  if (value === undefined || value === null) return undefined;
  if (value === "none") return null;
  const id = Number(value);
  return Number.isFinite(id) ? id : undefined;
}

/** 移到某个分组（右键菜单和拖放共用）。返回是不是真的移了。 */
export async function moveHostToGroup(host: Host, groupId: DropGroup): Promise<boolean> {
  if (host.group_id === groupId) return false;
  const { groups, saveHost } = useVault.getState();
  const label = groupId === null ? ungroupedLabel(isUngroupedFlat()) : `「${groups.find((g) => g.id === groupId)?.name ?? "分组"}」`;
  try {
    await saveHost({ ...draftOfHost(host), group_id: groupId });
    toast.success(`「${host.name}」已移到${label}`);
    return true;
  } catch (error) {
    toast.error(`移动失败：${errorText(error)}`);
    return false;
  }
}

const sameIds = (a: number[], b: number[]) => a.length === b.length && a.every((id, index) => id === b[index]);

/**
 * 把主机放到 `targetId` 那台主机的前面 / 后面。目标在别的分组里就先换过去（`sort` 由后端置 0），
 * 再按松手的位置给目标那一层整层重新编号。位置没变就什么都不做。
 */
export async function dropHostAt(host: Host, targetId: number, after: boolean): Promise<void> {
  const { hosts, reorderHosts } = useVault.getState();
  const target = hosts.find((item) => item.id === targetId);
  if (!target) return;
  const layer = hostLayerIds(hosts, target.group_id);
  const ids = insertInLayer(layer, host.id, targetId, after);
  if (!ids) return;
  const sameGroup = host.group_id === target.group_id;
  if (sameGroup && sameIds(ids, layer)) return;
  if (!sameGroup && !(await moveHostToGroup(host, target.group_id))) return;
  try {
    await reorderHosts(ids);
  } catch (error) {
    toast.error(`调整顺序失败：${errorText(error)}`);
  }
}

/** 把分组放到同一个父分组下 `targetId` 那个分组的前面 / 后面。 */
export async function dropGroupAt(group: Group, targetId: number, after: boolean): Promise<void> {
  const { groups, reorderGroups } = useVault.getState();
  const target = groups.find((item) => item.id === targetId);
  if (!target || target.parent_id !== group.parent_id) return;
  const layer = groupLayerIds(groups, group.parent_id);
  const ids = insertInLayer(layer, group.id, targetId, after);
  if (!ids || sameIds(ids, layer)) return;
  try {
    await reorderGroups(ids);
  } catch (error) {
    toast.error(`调整顺序失败：${errorText(error)}`);
  }
}

interface Probe {
  over: DropGroup | undefined;
  insert: InsertMark | null;
  /** 指针在右边的卡片上，但现在不是手动排序：松手时提示一次（不响应）。 */
  blockedByMode?: boolean;
}

interface DragSpec {
  kind: "host" | "group";
  id: number;
  label: string;
  probe: (target: Element | null, y: number, x: number) => Probe;
  commit: (probe: Probe) => void;
}

const isLocked = (el: Element) => el.closest('[data-order-locked="true"]') !== null;

/** 指针在这一行的下半部分吗。 */
function inLowerHalf(row: Element, y: number): boolean {
  const rect = row.getBoundingClientRect();
  return y >= rect.top + rect.height / 2;
}

/** 指针在这张卡片的右半部分吗（网格里卡片是左右并排的）。 */
function inRightHalf(card: Element, x: number): boolean {
  const rect = card.getBoundingClientRect();
  return x >= rect.left + rect.width / 2;
}

let modeHintShown = false;

/** 不是手动排序时想在右边拖动：提示一次（同一次页面内最多一次）。 */
function showModeHint() {
  if (modeHintShown) return;
  modeHintShown = true;
  toast.info("切换到「手动」排序后才能拖动");
}

/** 测试用：清掉「已经提示过」的记号。 */
export function resetModeHintForTests() {
  modeHintShown = false;
}

/** 复制一张卡片的样子给拖动用：去掉所有 data-* / id / tabindex，免得和真的卡片混淆。 */
function makeCardGhost(source: HTMLElement, start: { x: number; y: number }): DragGhostInfo {
  const rect = source.getBoundingClientRect();
  const clone = source.cloneNode(true) as HTMLElement;
  const strip = (el: Element) => {
    for (const name of el.getAttributeNames()) {
      if (name.startsWith("data-") || name === "id" || name === "tabindex") el.removeAttribute(name);
    }
  };
  strip(clone);
  clone.querySelectorAll("*").forEach(strip);
  clone.style.opacity = "";
  return {
    html: clone.outerHTML,
    width: rect.width,
    height: rect.height,
    offsetX: start.x - rect.left,
    offsetY: start.y - rect.top,
  };
}

/** 按下鼠标后：拖过门槛才算拖，不然就是普通单击 / 双击。 */
function beginDrag(spec: DragSpec, event: React.PointerEvent) {
  const start = { x: event.clientX, y: event.clientY };
  const source = event.currentTarget as HTMLElement;
  let active = false;
  let ghost: DragGhostInfo | null = null;
  let sourceOpacity: string | null = null;
  let probed: Probe = { over: undefined, insert: null };

  const move = (e: PointerEvent) => {
    if (!active) {
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < DRAG_THRESHOLD) return;
      active = true;
      document.body.classList.add("zz-host-dragging");
      // 右边的卡片 / 行：整张卡片的影子跟着指针走，原来的那张变淡留在原地
      if (spec.kind === "host" && source.hasAttribute?.("data-page-host")) {
        ghost = makeCardGhost(source, start);
        sourceOpacity = source.style.opacity;
        source.style.opacity = "0.35";
      }
    }
    probed = spec.probe(document.elementFromPoint(e.clientX, e.clientY), e.clientY, e.clientX);
    useHostDrag.setState({
      drag: {
        kind: spec.kind,
        id: spec.id,
        label: spec.label,
        x: e.clientX,
        y: e.clientY,
        over: probed.over,
        insert: probed.insert,
        ghost,
      },
    });
  };
  const finish = (commit: boolean) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", cancel);
    window.removeEventListener("keydown", key, true);
    if (!active) return;
    document.body.classList.remove("zz-host-dragging");
    if (sourceOpacity !== null) source.style.opacity = sourceOpacity;
    useHostDrag.setState({ drag: null });
    suppressClick = true;
    window.setTimeout(() => (suppressClick = false), 0);
    if (commit) spec.commit(probed);
  };
  const up = () => finish(true);
  const cancel = () => finish(false);
  const key = (e: KeyboardEvent) => {
    if (e.key === "Escape") finish(false);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", cancel);
  window.addEventListener("keydown", key, true);
}

/** 主机上按下鼠标时调用：拖过门槛才算拖，不然就是普通单击 / 双击。 */
export function startHostDrag(host: Host, event: React.PointerEvent) {
  if (event.button !== 0) return;
  // 卡片里面的按钮（"连接"之类）不是拖的把手
  const button = (event.target as HTMLElement).closest("button");
  if (button && button !== event.currentTarget) return;
  beginDrag(
    {
      kind: "host",
      id: host.id,
      label: host.name,
      probe: (el, y, x) => {
        // 右边的卡片 / 行：只管同一分组内的排序，不是换分组的目标
        const pageCard = el?.closest("[data-page-host]");
        if (pageCard) {
          const container = pageCard.closest("[data-page-order]");
          const state = container?.getAttribute("data-page-order");
          const targetId = Number(pageCard.getAttribute("data-page-host"));
          if (!Number.isFinite(targetId) || targetId === host.id) return { over: undefined, insert: null };
          if (state === "mode") return { over: undefined, insert: null, blockedByMode: true };
          // 别的分组的主机也可以放：`dropHostAt` 会先换分组再按位置排（提示标签会写「→ 分组名」）
          if (state !== "manual") return { over: undefined, insert: null };
          const after =
            container?.getAttribute("data-page-layout") === "grid"
              ? inRightHalf(pageCard, x)
              : inLowerHalf(pageCard, y);
          return { over: undefined, insert: { kind: "host", id: targetId, after, scope: "page" } };
        }
        let over = parseTarget(el?.closest("[data-drop-group]")?.getAttribute("data-drop-group"));
        // 平铺模式：树的空白处（不在任何一行 / 按钮上）= 顶层
        if (over === undefined && el && !el.closest("button") && el.closest("[data-drop-blank]")) over = null;
        const row = el?.closest("[data-order-host]");
        const targetId = Number(row?.getAttribute("data-order-host"));
        if (!row || isLocked(row) || !Number.isFinite(targetId) || targetId === host.id) {
          return { over, insert: null };
        }
        return { over, insert: { kind: "host", id: targetId, after: inLowerHalf(row, y) } };
      },
      commit: ({ over, insert, blockedByMode }) => {
        if (insert) void dropHostAt(host, insert.id, insert.after);
        else if (over !== undefined) void moveHostToGroup(host, over);
        else if (blockedByMode) showModeHint();
      },
    },
    event,
  );
}

/** 分组行上按下鼠标时调用：只能在同一个父分组下调整顺序；搜索时不能拖。 */
export function startGroupDrag(group: Group, event: React.PointerEvent) {
  if (event.button !== 0) return;
  if (isLocked(event.currentTarget as Element)) return;
  const parent = String(group.parent_id ?? "root");
  beginDrag(
    {
      kind: "group",
      id: group.id,
      label: group.name,
      probe: (el, y) => {
        const row = el?.closest("[data-order-group]");
        const targetId = Number(row?.getAttribute("data-order-group"));
        if (
          !row ||
          isLocked(row) ||
          row.getAttribute("data-order-parent") !== parent ||
          !Number.isFinite(targetId) ||
          targetId === group.id
        ) {
          return { over: undefined, insert: null };
        }
        return { over: undefined, insert: { kind: "group", id: targetId, after: inLowerHalf(row, y) } };
      },
      commit: ({ insert }) => {
        if (insert) void dropGroupAt(group, insert.id, insert.after);
      },
    },
    event,
  );
}

/** 这一行是不是正被拖着悬停的换分组目标（高亮用）；拖的是分组、或指针在排序位置上时不高亮。 */
export function useIsDropTarget(target: DropGroup): boolean {
  return useHostDrag(
    (state) =>
      state.drag !== null && state.drag.kind === "host" && state.drag.insert === null && state.drag.over === target,
  );
}

/** 这一行前面 / 后面有没有插入线。 */
export function useInsertMark(
  kind: "host" | "group",
  id: number,
  scope: "tree" | "page" = "tree",
): "before" | "after" | null {
  return useHostDrag((state) => {
    const insert = state.drag?.insert;
    if (!insert || insert.kind !== kind || insert.id !== id || (insert.scope ?? "tree") !== scope) return null;
    return insert.after ? "after" : "before";
  });
}

/** 一条细的插入线，画在带 `relative` 的行里面。 */
export function InsertLine({
  mark,
  vertical = false,
}: {
  mark: "before" | "after" | null;
  /** 网格里卡片左右并排：画竖线（前面 = 左边缘、后面 = 右边缘）。 */
  vertical?: boolean;
}) {
  if (!mark) return null;
  return (
    <span
      data-section="drop-line"
      data-position={mark}
      data-orientation={vertical ? "vertical" : undefined}
      className={cn(
        "pointer-events-none absolute z-10 rounded-full bg-primary",
        vertical
          ? cn("inset-y-1 w-0.5", mark === "before" ? "left-0" : "right-0")
          : cn("inset-x-1 h-0.5", mark === "before" ? "top-0" : "bottom-0"),
      )}
    />
  );
}

/** 跟着指针走的小标签：拖的是哪台、松手会放到哪。 */
export function HostDragGhost() {
  const drag = useHostDrag((state) => state.drag);
  const groups = useVault((state) => state.groups);
  const hosts = useVault((state) => state.hosts);
  const flat = useUngroupedFlat();
  if (!drag) return null;
  const rootLabel = ungroupedLabel(flat);
  const Icon = drag.kind === "group" ? Folder : Server;
  let text: string;
  if (drag.kind === "group") {
    text = drag.insert ? `${drag.label}（调整顺序）` : `${drag.label}（在同一层拖动调整顺序）`;
  } else if (drag.insert) {
    // 插到别的分组里的主机之间：说明会换分组
    const target = hosts.find((host) => host.id === drag.insert?.id);
    const moved = target !== undefined && hosts.find((host) => host.id === drag.id)?.group_id !== target.group_id;
    const groupName =
      target?.group_id == null ? rootLabel : (groups.find((g) => g.id === target.group_id)?.name ?? "分组");
    text = moved ? `${drag.label} → ${groupName}` : `${drag.label}（调整顺序）`;
  } else {
    const name =
      drag.over === undefined
        ? null
        : drag.over === null
          ? rootLabel
          : (groups.find((group) => group.id === drag.over)?.name ?? null);
    text = name ? `${drag.label} → ${name}` : `${drag.label}（拖到左边的分组上）`;
  }
  const card = drag.ghost;
  return (
    <>
      {card && (
        <div
          data-section="host-drag-card"
          aria-hidden="true"
          className="pointer-events-none fixed z-[300000] overflow-hidden rounded-lg opacity-90 shadow-xl ring-1 ring-primary/60 [&>*]:h-full [&>*]:w-full"
          style={{
            left: drag.x - card.offsetX,
            top: drag.y - card.offsetY,
            width: card.width,
            height: card.height,
          }}
          dangerouslySetInnerHTML={{ __html: card.html }}
        />
      )}
      <div
        data-section="host-drag-ghost"
        className="pointer-events-none fixed z-[300001] flex items-center gap-1.5 rounded-md border border-primary/60 bg-background/95 px-2 py-1 text-xs shadow-lg"
        // 有卡片影子时，提示小标签挂在影子下面，不盖住它
        style={
          card
            ? { left: drag.x - card.offsetX, top: drag.y - card.offsetY + card.height + 6 }
            : { left: drag.x + 12, top: drag.y + 8 }
        }
      >
        <Icon size={12} className="text-muted-foreground" />
        {text}
      </div>
    </>
  );
}
