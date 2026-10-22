//! 拖主机换分组：按住主机（树、卡片、列表都行）拖到左边树里的分组上松手。
//!
//! 用指针事件自己做：Tauri 默认开着 dragDropEnabled，Windows 上 WebView 里的 HTML5 拖放用不了
//! （标签栏拖 SFTP 也是这么做的）。放的目标是带 `data-drop-group` 的行：分组行是那个分组，
//! "未分组"行是 `none`，树里的主机行是它所在的分组（拖到同组的主机上等于放进那个组）。

import { Server } from "lucide-react";
import { create } from "zustand";

import { draftOfHost, type Host } from "../../api/vault";
import { useVault } from "../../stores/vault";
import { toast } from "../ui/toast";

const DRAG_THRESHOLD = 6;

/** 放到哪：分组 id，或者未分组。 */
export type DropGroup = number | null;

interface DragState {
  hostId: number;
  label: string;
  x: number;
  y: number;
  /** 指针下面的目标；undefined = 不在任何目标上 */
  over: DropGroup | undefined;
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

/** 移到某个分组（右键菜单和拖放共用）。 */
export async function moveHostToGroup(host: Host, groupId: DropGroup): Promise<void> {
  if (host.group_id === groupId) return;
  const { groups, saveHost } = useVault.getState();
  const label = groupId === null ? "未分组" : `「${groups.find((g) => g.id === groupId)?.name ?? "分组"}」`;
  try {
    await saveHost({ ...draftOfHost(host), group_id: groupId });
    toast.success(`「${host.name}」已移到${label}`);
  } catch (error) {
    toast.error(`移动失败：${error instanceof Error ? error.message : String(error)}`);
  }
}

/** 主机上按下鼠标时调用：拖过门槛才算拖，不然就是普通单击 / 双击。 */
export function startHostDrag(host: Host, event: React.PointerEvent) {
  if (event.button !== 0) return;
  // 卡片里面的按钮（"连接"之类）不是拖的把手
  const button = (event.target as HTMLElement).closest("button");
  if (button && button !== event.currentTarget) return;
  const start = { x: event.clientX, y: event.clientY };
  let active = false;
  let over: DropGroup | undefined;

  const probe = (x: number, y: number): DropGroup | undefined =>
    parseTarget(document.elementFromPoint(x, y)?.closest("[data-drop-group]")?.getAttribute("data-drop-group"));

  const move = (e: PointerEvent) => {
    if (!active) {
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < DRAG_THRESHOLD) return;
      active = true;
      document.body.classList.add("zz-host-dragging");
    }
    over = probe(e.clientX, e.clientY);
    useHostDrag.setState({ drag: { hostId: host.id, label: host.name, x: e.clientX, y: e.clientY, over } });
  };
  const finish = (commit: boolean) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", cancel);
    window.removeEventListener("keydown", key, true);
    if (!active) return;
    document.body.classList.remove("zz-host-dragging");
    useHostDrag.setState({ drag: null });
    suppressClick = true;
    window.setTimeout(() => (suppressClick = false), 0);
    if (commit && over !== undefined) void moveHostToGroup(host, over);
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

/** 这一行是不是正被拖着悬停的目标（高亮用）。 */
export function useIsDropTarget(target: DropGroup): boolean {
  return useHostDrag((state) => state.drag !== null && state.drag.over === target);
}

/** 跟着指针走的小标签：拖的是哪台、松手会放到哪。 */
export function HostDragGhost() {
  const drag = useHostDrag((state) => state.drag);
  const groups = useVault((state) => state.groups);
  if (!drag) return null;
  const target =
    drag.over === undefined
      ? null
      : drag.over === null
        ? "未分组"
        : groups.find((group) => group.id === drag.over)?.name ?? null;
  return (
    <div
      data-section="host-drag-ghost"
      className="pointer-events-none fixed z-[300000] flex items-center gap-1.5 rounded-md border border-primary/60 bg-background/95 px-2 py-1 text-xs shadow-lg"
      style={{ left: drag.x + 12, top: drag.y + 8 }}
    >
      <Server size={12} className="text-muted-foreground" />
      {target ? `${drag.label} → ${target}` : `${drag.label}（拖到左边的分组上）`}
    </div>
  );
}
