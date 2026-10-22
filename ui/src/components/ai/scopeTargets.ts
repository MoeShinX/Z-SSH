//! AI 对话的目标列表（AI-SCOPE-1、AI-SCOPE-2）：当前标签的会话 + 用户 @ 的主机 + 切焦点时
//! 自动保留的会话，**按主机建模**后同步给后端作用范围。纯函数，方便单测。
//!
//! 目标 ID 是稳定的逻辑 ID：主机库里的主机一律 `host-<hostId>`（开几个终端、切不切标签都
//! 不变——后端把"目标被移出"当作在途请求失效的信号，ID 不能随切换变化）；快速连接这类
//! 没有主机记录的会话用会话 ID。每个逻辑目标再带一个**绑定会话**，执行时优先用它。
import type { ScopeTargetChange } from "../../api/agent";
import { hostTargetId, parseHostTargetId } from "./aiHelpers";

/** 当前标签里一个已经有会话 ID 的 pane。 */
export interface ScopeSession {
  sessionId: string;
  hostId: number | null;
  hostLabel: string;
  username: string;
  /** 键盘焦点所在的 pane（整个标签里至多一个）；绑定会话优先取它。 */
  focused?: boolean;
}

/**
 * 留在对话范围里的一台主机：用户 @ 进来的，或者焦点切走时自动保留的原来那台。
 *
 * - 有主机记录（`hostId`）：目标是 `host-<hostId>`；`sessionId` 只是自动保留时记下的原会话
 *   （它关了，保留的目标跟着消失）；
 * - 没有主机记录（快速连接）：目标是会话 ID，`sessionId` 必填。
 */
export interface PinnedHost {
  /** 主机库里的主机；快速连接的会话没有。 */
  hostId: number | null;
  name: string;
  username: string;
  sessionId?: string;
  /** 焦点切走时自动保留下来的（不是用户 @ 的）：它的会话一关就跟着消失。 */
  retained?: boolean;
}

/** 每台主机最近一次获得焦点的会话（主机 id → 会话 ID）。 */
export type RecentFocus = ReadonlyMap<number, string>;

/** 一个会话的逻辑目标 ID：有主机记录是 `host-<id>`，否则是会话 ID。 */
export function logicalTargetId(session: Pick<ScopeSession, "sessionId" | "hostId">): string {
  return session.hostId !== null ? hostTargetId(session.hostId) : session.sessionId;
}

/** 一个固定目标的逻辑目标 ID（也是 chip 的 key）。 */
export function pinKey(pin: PinnedHost): string {
  if (pin.hostId !== null) return hostTargetId(pin.hostId);
  return pin.sessionId ?? `pin-${pin.name}`;
}

/** 固定目标是不是已经被"当前"那一条代表了（chip 上不再单独显示）。 */
export function pinIsCurrent(pin: PinnedHost, current: ScopeSession | undefined): boolean {
  if (!current) return false;
  if (pin.hostId !== null) return pin.hostId === current.hostId;
  return pin.sessionId !== undefined && pin.sessionId === current.sessionId;
}

/**
 * 拼目标列表。**一台主机只有一个逻辑目标**（`host-<id>`），不管它开了几个会话：
 * - 绑定会话：这台主机的焦点会话 → 最近一次获得焦点的、还开着的会话 → 当前标签里它的
 *   第一个会话 → 保留时记下的原会话；都没有（@ 了没打开的主机）为空；
 * - 当前（`current`）：当前标签第一个会话所属的目标；
 * - 固定（`pinned`）：@ 的或自动保留的，任何一个来源是就是。
 *
 * `open` 是界面里所有还开着的会话 ID（决定绑定的会话 / 保留的会话是不是已经关了）。
 */
export function buildScopeTargets(
  sessions: ScopeSession[],
  pinned: PinnedHost[],
  open?: ReadonlySet<string>,
  recent?: RecentFocus,
): ScopeTargetChange[] {
  const inTab = new Set(sessions.map((s) => s.sessionId));
  const alive = (id: string): boolean => inTab.has(id) || open === undefined || open.has(id);

  interface Entry {
    id: string;
    label: string;
    username: string;
    hostId: number | null;
    tabSessions: ScopeSession[];
    pin: PinnedHost | undefined;
  }
  const entries = new Map<string, Entry>();
  for (const s of sessions) {
    const id = logicalTargetId(s);
    const entry = entries.get(id);
    if (entry) entry.tabSessions.push(s);
    else {
      entries.set(id, {
        id,
        label: s.hostLabel,
        username: s.username,
        hostId: s.hostId,
        tabSessions: [s],
        pin: undefined,
      });
    }
  }
  for (const pin of pinned) {
    const id = pinKey(pin);
    const entry = entries.get(id);
    if (entry) {
      entry.pin ??= pin;
      continue;
    }
    // 自动保留的目标跟着它的会话走：会话关了就不再留（这台主机最近焦点的会话还开着除外）。
    if (pin.retained && !retainedAlive(pin, alive, recent)) continue;
    if (!pin.retained && pin.hostId === null) continue;
    entries.set(id, {
      id,
      label: pin.name,
      username: pin.username,
      hostId: pin.hostId,
      tabSessions: [],
      pin,
    });
  }

  const currentId = sessions[0] ? logicalTargetId(sessions[0]) : null;
  return [...entries.values()].map((entry) => {
    const session = boundSession(entry, alive, recent);
    return {
      target_id: entry.id,
      host_label: entry.label,
      username: entry.username,
      status: session ? "connected" : "pinned",
      current: entry.id === currentId,
      pinned: entry.pin !== undefined,
      session_id: session,
    };
  });
}

/** 自动保留的目标还该不该留：它记下的会话还开着，或这台主机最近获得焦点的会话还开着。 */
function retainedAlive(
  pin: PinnedHost,
  alive: (id: string) => boolean,
  recent: RecentFocus | undefined,
): boolean {
  if (pin.sessionId !== undefined && alive(pin.sessionId)) return true;
  const last = pin.hostId !== null ? recent?.get(pin.hostId) : undefined;
  return last !== undefined && alive(last);
}

/** 一个逻辑目标这一刻该绑哪个会话（见 [`buildScopeTargets`]）。 */
function boundSession(
  entry: {
    hostId: number | null;
    tabSessions: ScopeSession[];
    pin: PinnedHost | undefined;
  },
  alive: (id: string) => boolean,
  recent: RecentFocus | undefined,
): string | null {
  const focused = entry.tabSessions.find((s) => s.focused);
  if (focused) return focused.sessionId;
  if (entry.hostId !== null) {
    const last = recent?.get(entry.hostId);
    if (last !== undefined && alive(last)) return last;
  }
  const first = entry.tabSessions[0];
  if (first) return first.sessionId;
  const kept = entry.pin?.sessionId;
  return kept !== undefined && alive(kept) ? kept : null;
}

/**
 * 焦点切走时保留原来的"当前"：对话进行中，原来那个会话还开着、又没被代表，就记成一个
 * 自动保留的固定目标（显示成可以 × 掉的标签）。新的"当前"和它是同一台主机时什么都不用做
 * （主机级的目标没变，只是绑定的会话跟着焦点走）。
 */
export function retainPrevious(
  pinned: PinnedHost[],
  previous: ScopeSession | null,
  open?: ReadonlySet<string>,
  current?: ScopeSession | null,
): PinnedHost[] {
  if (!previous) return pinned;
  if (open && !open.has(previous.sessionId)) return pinned;
  if (previous.hostId !== null && previous.hostId === current?.hostId) return pinned;
  const covered = pinned.some((pin) =>
    previous.hostId !== null
      ? pin.hostId === previous.hostId
      : pin.sessionId === previous.sessionId,
  );
  if (covered) return pinned;
  return [
    ...pinned,
    {
      hostId: previous.hostId,
      name: previous.hostLabel,
      username: previous.username,
      sessionId: previous.sessionId,
      retained: true,
    },
  ];
}

/** 去掉会话已经关了的自动保留目标。 */
export function pruneRetained(
  pinned: PinnedHost[],
  open?: ReadonlySet<string>,
  recent?: RecentFocus,
): PinnedHost[] {
  if (!open) return pinned;
  const alive = (id: string): boolean => open.has(id);
  const next = pinned.filter(
    (pin) => !(pin.retained && pin.sessionId && !retainedAlive(pin, alive, recent)),
  );
  return next.length === pinned.length ? pinned : next;
}

/** @ 一台主机后的固定目标列表，以及这次要不要提示"已经在对话里了"。 */
export function pinHost(
  pinned: PinnedHost[],
  host: { hostId: number; name: string; username: string },
  sessions: ScopeSession[],
): { pinned: PinnedHost[]; alreadyIn: boolean } {
  const existing = pinned.findIndex((pin) => pin.hostId === host.hostId);
  if (existing >= 0) {
    const pin = pinned[existing];
    // 自动保留的被用户明确 @ 了：从此是用户固定的（会话关了也不消失）。
    if (pin.retained) {
      const next = pinned.slice();
      next[existing] = { ...pin, retained: false };
      return { pinned: next, alreadyIn: false };
    }
    return { pinned, alreadyIn: true };
  }
  const entry: PinnedHost = { hostId: host.hostId, name: host.name, username: host.username };
  // 当前标签里已经有这台主机的会话：目标本来就在列表里，只记一个固定标记（切走焦点后它仍留着）。
  return {
    pinned: [...pinned, entry],
    alreadyIn: sessions.some((s) => s.hostId === host.hostId),
  };
}

/** 对话里出现过的目标（旧式 `session_id` 输入的卡片据此显示主机名；会话关了也留着）。 */
export interface KnownTarget {
  label: string;
  hostId: number | null;
  sessionId: string | null;
}

/** 把当前的目标列表登记进去（同一个 ID 以最新的为准）。 */
export function registerTargets(
  known: Map<string, KnownTarget>,
  targets: readonly ScopeTargetChange[],
): void {
  for (const target of targets) {
    const hostId = parseHostTargetId(target.target_id);
    known.set(target.target_id, {
      label: target.host_label,
      hostId,
      sessionId: target.session_id ?? (hostId === null ? target.target_id : null),
    });
  }
}
