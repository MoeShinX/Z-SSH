//! AI 对话的目标列表（AI-SCOPE-1）：当前标签的会话 + 用户 @ 的主机 + 切焦点时自动保留的会话，
//! 按主机去重后同步给后端作用范围。纯函数，方便单测。
import type { ScopeTargetChange } from "../../api/agent";
import type { CallTarget } from "./chatHistory";
import {
  hostTargetId,
  parseHostTargetId,
  sessionIdOf,
  targetIdOf,
  toolHasTarget,
} from "./aiHelpers";

/** 当前标签里一个已经有会话 ID 的 pane。 */
export interface ScopeSession {
  sessionId: string;
  hostId: number | null;
  hostLabel: string;
  username: string;
}

/**
 * 留在对话范围里的一台主机：用户 @ 进来的，或者焦点切走时自动保留的原来那台。
 *
 * 目标 ID 一旦进了范围就不再变（后端把"目标被移出"当作在途请求失效的信号）：
 * - 有 `sessionId`（绑定了一个具体会话）：一直用会话 ID，会话还开着就一直在范围里；
 * - 没有：用 `host-<id>`，后端自己找这台主机已有的会话，没有就自动连接。
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

/** 一个固定目标在目标列表里的 ID。 */
export function pinKey(pin: PinnedHost): string {
  if (pin.sessionId) return pin.sessionId;
  return pin.hostId !== null ? hostTargetId(pin.hostId) : `pin-${pin.name}`;
}

/** 这个固定目标的会话还开着吗（没给全局会话表就当作开着）。 */
function boundAlive(pin: PinnedHost, open: ReadonlySet<string> | undefined): boolean {
  if (!pin.sessionId) return false;
  return open === undefined || open.has(pin.sessionId);
}

/** 固定目标是不是已经被"当前"那一条代表了（chip 上不再单独显示）。 */
export function pinIsCurrent(pin: PinnedHost, current: ScopeSession | undefined): boolean {
  if (!current) return false;
  if (pin.sessionId) return pin.sessionId === current.sessionId;
  return pin.hostId !== null && pin.hostId === current.hostId;
}

/**
 * 拼目标列表。同一台主机只出现一次：
 * - 固定目标和当前标签里的会话是同一台 → 保留会话那一条并标 `pinned`；
 * - 固定目标没绑会话（`host-<id>`）而当前标签里正好有这台主机的会话 → 只留 `host-<id>`
 *   这一条（ID 不变，在途请求不受影响），"当前"标记转给它。
 *
 * `open` 是界面里所有还开着的会话 ID（决定绑定的会话是不是已经关了）。
 */
export function buildScopeTargets(
  sessions: ScopeSession[],
  pinned: PinnedHost[],
  open?: ReadonlySet<string>,
): ScopeTargetChange[] {
  const inTab = new Set(sessions.map((s) => s.sessionId));
  const boundIds = new Set<string>();
  const unboundHosts = new Set<number>();
  const extras: ScopeTargetChange[] = [];
  const seen = new Set<string>();

  for (const pin of pinned) {
    if (pin.sessionId && (inTab.has(pin.sessionId) || boundAlive(pin, open))) {
      boundIds.add(pin.sessionId);
      continue;
    }
    // 绑定的会话关了：用户 @ 的退回按主机；自动保留的跟着消失。
    if (pin.retained || pin.hostId === null) continue;
    unboundHosts.add(pin.hostId);
  }

  const current = sessions[0];
  const targets: ScopeTargetChange[] = [];
  sessions.forEach((s, index) => {
    const merged =
      s.hostId !== null && unboundHosts.has(s.hostId) && !boundIds.has(s.sessionId);
    if (merged || seen.has(s.sessionId)) return;
    seen.add(s.sessionId);
    targets.push({
      target_id: s.sessionId,
      host_label: s.hostLabel,
      username: s.username,
      status: "connected",
      current: index === 0,
      pinned: boundIds.has(s.sessionId),
    });
  });

  for (const pin of pinned) {
    if (pin.sessionId && boundIds.has(pin.sessionId)) {
      if (inTab.has(pin.sessionId) || seen.has(pin.sessionId)) continue;
      seen.add(pin.sessionId);
      extras.push({
        target_id: pin.sessionId,
        host_label: pin.name,
        username: pin.username,
        status: "connected",
        current: false,
        pinned: true,
      });
    } else if (pin.hostId !== null && unboundHosts.has(pin.hostId)) {
      const id = hostTargetId(pin.hostId);
      if (seen.has(id)) continue;
      seen.add(id);
      extras.push({
        target_id: id,
        host_label: pin.name,
        username: pin.username,
        status: "pinned",
        // 当前标签里"当前"那个会话属于这台主机：它被并进来了，"当前"跟着过来
        current: current !== undefined && current.hostId === pin.hostId,
        pinned: true,
      });
    }
  }
  return [...targets, ...extras];
}

/**
 * 焦点切走时保留原来的"当前"：对话进行中，原来那个会话还开着、又没被代表，就记成一个
 * 自动保留的固定目标（显示成可以 × 掉的标签）。
 */
export function retainPrevious(
  pinned: PinnedHost[],
  previous: ScopeSession | null,
  open?: ReadonlySet<string>,
): PinnedHost[] {
  if (!previous) return pinned;
  if (open && !open.has(previous.sessionId)) return pinned;
  const covered = pinned.some((pin) =>
    pin.sessionId
      ? pin.sessionId === previous.sessionId
      : pin.hostId !== null && pin.hostId === previous.hostId,
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
export function pruneRetained(pinned: PinnedHost[], open?: ReadonlySet<string>): PinnedHost[] {
  if (!open) return pinned;
  const next = pinned.filter(
    (pin) => !(pin.retained && pin.sessionId && !open.has(pin.sessionId)),
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
  // 当前标签里已经有这台主机的会话：绑在那个会话上，目标列表里只留会话那一条（标 pinned）。
  const bound = sessions.find((s) => s.hostId === host.hostId);
  const entry: PinnedHost = {
    hostId: host.hostId,
    name: host.name,
    username: host.username,
    ...(bound ? { sessionId: bound.sessionId } : {}),
  };
  return { pinned: [...pinned, entry], alreadyIn: bound !== undefined };
}

/** 对话里出现过的目标（工具卡片据此标主机名、跳到终端；会话关了也留着）。 */
export interface KnownTarget {
  label: string;
  hostId: number | null;
  sessionId: string | null;
}

/** 把当前的会话和固定目标登记进去（同一个 ID 以最新的为准）。 */
export function registerTargets(
  known: Map<string, KnownTarget>,
  sessions: ScopeSession[],
  pinned: PinnedHost[],
): void {
  for (const s of sessions) {
    known.set(s.sessionId, { label: s.hostLabel, hostId: s.hostId, sessionId: s.sessionId });
  }
  for (const pin of pinned) {
    if (pin.sessionId) {
      known.set(pin.sessionId, { label: pin.name, hostId: pin.hostId, sessionId: pin.sessionId });
    }
    if (pin.hostId !== null) {
      known.set(hostTargetId(pin.hostId), {
        label: pin.name,
        hostId: pin.hostId,
        sessionId: pin.sessionId ?? null,
      });
    }
  }
}

/**
 * 一次工具调用作用在哪个目标上：输入里写了 `target_id` 就用它，没写是当时的"当前"。
 * 返回 null = 这个工具不带目标，或者目标不认识。
 */
export function describeCallTarget(
  tool: string,
  input: unknown,
  known: ReadonlyMap<string, KnownTarget>,
  currentId: string | null,
  targetCount: number,
): CallTarget | null {
  if (!toolHasTarget(tool)) return null;
  const id = targetIdOf(input) ?? sessionIdOf(input) ?? currentId;
  if (!id) return null;
  const hit = known.get(id);
  if (!hit) return null;
  return {
    id,
    label: hit.label,
    hostId: hit.hostId ?? parseHostTargetId(id),
    sessionId: hit.sessionId,
    multi: targetCount > 1,
  };
}
