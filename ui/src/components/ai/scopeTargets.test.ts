//! AI 目标列表（AI-SCOPE-1、AI-SCOPE-2）：一台主机一个逻辑目标、绑定会话、切焦点保留。

import { describe, expect, it } from "vitest";

import {
  buildScopeTargets,
  logicalTargetId,
  pinHost,
  pinIsCurrent,
  pinKey,
  pruneRetained,
  registerTargets,
  retainPrevious,
  type KnownTarget,
  type PinnedHost,
  type ScopeSession,
} from "./scopeTargets";

const web: ScopeSession = { sessionId: "s-web", hostId: 3, hostLabel: "web-1", username: "root" };
const web2: ScopeSession = { ...web, sessionId: "s-web2" };
const db: ScopeSession = { sessionId: "s-db", hostId: 4, hostLabel: "db-1", username: "root" };
const quick: ScopeSession = { sessionId: "s-q", hostId: null, hostLabel: "临时", username: "u" };

const ids = (list: { target_id: string }[]) => list.map((t) => t.target_id);
const byId = (list: ReturnType<typeof buildScopeTargets>, id: string) =>
  list.find((t) => t.target_id === id);

describe("一台主机一个逻辑目标", () => {
  it("有主机记录的会话一律用 host-<id>，快速连接的仍用会话 ID", () => {
    expect(logicalTargetId(web)).toBe("host-3");
    expect(logicalTargetId(quick)).toBe("s-q");
    const targets = buildScopeTargets([web, db, quick], []);
    expect(ids(targets)).toEqual(["host-3", "host-4", "s-q"]);
    expect(targets.map((t) => t.current)).toEqual([true, false, false]);
    expect(targets.map((t) => t.session_id)).toEqual(["s-web", "s-db", "s-q"]);
    expect(targets.every((t) => !t.pinned && t.status === "connected")).toBe(true);
  });

  it("同一台主机的两个会话（分屏 / 两个标签）合成一个目标，绑定焦点所在的那个", () => {
    const a = { ...web, focused: false };
    const b = { ...web2, focused: true };
    const targets = buildScopeTargets([a, b], []);
    expect(ids(targets)).toEqual(["host-3"]);
    expect(targets[0]).toMatchObject({ session_id: "s-web2", current: true });
    // 焦点换到另一个窗格：同一个目标，绑定跟着走
    const moved = buildScopeTargets([{ ...a, focused: true }, { ...b, focused: false }], []);
    expect(ids(moved)).toEqual(["host-3"]);
    expect(moved[0].session_id).toBe("s-web");
  });

  it("没有焦点信息时绑定当前标签里这台主机的第一个会话", () => {
    expect(buildScopeTargets([web, web2], [])[0].session_id).toBe("s-web");
  });

  it("这台主机不在当前标签：绑定它最近一次获得焦点的、还开着的会话", () => {
    const pinned: PinnedHost[] = [{ hostId: 3, name: "web-1", username: "root" }];
    const open = new Set(["s-web", "s-web2", "s-db"]);
    const recent = new Map([[3, "s-web2"]]);
    const targets = buildScopeTargets([db], pinned, open, recent);
    expect(ids(targets)).toEqual(["host-4", "host-3"]);
    expect(byId(targets, "host-3")).toMatchObject({
      session_id: "s-web2",
      status: "connected",
      current: false,
      pinned: true,
    });
    // 最近焦点的会话关了：绑定为空（后端再找这台主机别的已连会话 / 自动连接）
    const closed = buildScopeTargets([db], pinned, new Set(["s-web", "s-db"]), recent);
    expect(byId(closed, "host-3")).toMatchObject({ session_id: null, status: "pinned" });
  });

  it("@ 一台没打开的主机：host-<id> 目标，没有绑定会话", () => {
    const { pinned, alreadyIn } = pinHost([], { hostId: 9, name: "cache-1", username: "ops" }, [web]);
    expect(alreadyIn).toBe(false);
    const targets = buildScopeTargets([web], pinned);
    expect(ids(targets)).toEqual(["host-3", "host-9"]);
    expect(targets[1]).toMatchObject({
      pinned: true,
      current: false,
      status: "pinned",
      session_id: null,
    });
  });

  it("当前主机又被 @：仍是同一个目标，只多一个 pinned 标记", () => {
    const { pinned, alreadyIn } = pinHost([], { hostId: 3, name: "web-1", username: "root" }, [web, db]);
    expect(alreadyIn).toBe(true);
    const targets = buildScopeTargets([web, db], pinned);
    expect(ids(targets)).toEqual(["host-3", "host-4"]);
    expect(targets[0]).toMatchObject({ current: true, pinned: true });
    expect(targets[1].pinned).toBe(false);
  });

  it("先 @ 再切到那台主机的标签：目标 ID 不变，当前标记和绑定转给它", () => {
    const pinned: PinnedHost[] = [{ hostId: 9, name: "cache-1", username: "ops" }];
    const cache: ScopeSession = { sessionId: "s-cache", hostId: 9, hostLabel: "cache-1", username: "ops" };
    const before = buildScopeTargets([web], pinned);
    const after = buildScopeTargets([cache], pinned);
    expect(ids(before)).toContain("host-9");
    expect(ids(after)).toEqual(["host-9"]);
    expect(after[0]).toMatchObject({ current: true, pinned: true, session_id: "s-cache" });
  });

  it("同一台主机重复 @ 不会出现第二个目标", () => {
    const once = pinHost([], { hostId: 9, name: "cache-1", username: "ops" }, []);
    const twice = pinHost(once.pinned, { hostId: 9, name: "cache-1", username: "ops" }, []);
    expect(twice.pinned).toBe(once.pinned);
    expect(twice.alreadyIn).toBe(true);
  });
});

describe("同一台主机的两个标签来回切换", () => {
  it("目标 ID 始终是 host-<id>，绑定跟着当前标签的会话走", () => {
    const open = new Set(["s-web", "s-web2"]);
    const tab1 = [{ ...web, focused: true }];
    const tab2 = [{ ...web2, focused: true }];
    const recent = new Map<number, string>();
    const seen: (string | null | undefined)[] = [];
    for (const tab of [tab1, tab2, tab1, tab2]) {
      recent.set(3, tab[0].sessionId);
      const targets = buildScopeTargets(tab, [], open, recent);
      expect(ids(targets)).toEqual(["host-3"]);
      seen.push(targets[0].session_id);
    }
    expect(seen).toEqual(["s-web", "s-web2", "s-web", "s-web2"]);
  });

  it("切到同一台主机的另一个标签不产生保留标签", () => {
    expect(retainPrevious([], web, new Set(["s-web", "s-web2"]), web2)).toEqual([]);
  });
});

describe("切焦点保留（对话进行中）", () => {
  it("原来的当前转成固定目标：目标 ID 不变，仍在范围里，新焦点成为当前", () => {
    const kept = retainPrevious([], web, undefined, db);
    expect(kept).toHaveLength(1);
    expect(kept[0]).toMatchObject({ hostId: 3, sessionId: "s-web", retained: true });
    const targets = buildScopeTargets([db], kept);
    expect(ids(targets)).toEqual(["host-4", "host-3"]);
    expect(targets[0]).toMatchObject({ current: true, pinned: false });
    expect(targets[1]).toMatchObject({ current: false, pinned: true, session_id: "s-web" });
  });

  it("切回来后合并成一个，仍是同一个目标 ID", () => {
    const kept = retainPrevious([], web, undefined, db);
    const back = buildScopeTargets([web], kept);
    expect(ids(back)).toEqual(["host-3"]);
    expect(back[0]).toMatchObject({ current: true, pinned: true });
  });

  it("已经被 @ 代表的不重复保留", () => {
    const pinnedByHost: PinnedHost[] = [{ hostId: 3, name: "web-1", username: "root" }];
    expect(retainPrevious(pinnedByHost, web, undefined, db)).toBe(pinnedByHost);
  });

  it("会话已经关了就不保留；没有上一个也不保留", () => {
    expect(retainPrevious([], web, new Set(["s-db"]), db)).toEqual([]);
    expect(retainPrevious([], null)).toEqual([]);
  });

  it("快速连接的会话（没有主机 id）也能保留，目标 ID 是会话 ID", () => {
    const kept = retainPrevious([], quick, undefined, db);
    const targets = buildScopeTargets([db], kept);
    expect(ids(targets)).toEqual(["host-4", "s-q"]);
    expect(targets[1].pinned).toBe(true);
  });

  it("保留的会话关了：目标跟着消失，标签也清掉；这台主机最近焦点的会话还开着就留着", () => {
    const kept = retainPrevious([], web, undefined, db);
    const open = new Set(["s-db"]);
    expect(ids(buildScopeTargets([db], kept, open))).toEqual(["host-4"]);
    expect(pruneRetained(kept, open)).toEqual([]);
    // 还开着就不动
    expect(pruneRetained(kept, new Set(["s-web", "s-db"]))).toBe(kept);
    // 原会话关了，但同一台主机另一个会话（最近焦点）还开着
    const recent = new Map([[3, "s-web2"]]);
    const open2 = new Set(["s-web2", "s-db"]);
    expect(pruneRetained(kept, open2, recent)).toBe(kept);
    expect(byId(buildScopeTargets([db], kept, open2, recent), "host-3")?.session_id).toBe("s-web2");
  });

  it("用户 @ 的主机，会话关了仍在范围里（按主机，没有绑定）", () => {
    const { pinned } = pinHost([], { hostId: 3, name: "web-1", username: "root" }, [web]);
    const targets = buildScopeTargets([db], pinned, new Set(["s-db"]));
    expect(ids(targets)).toEqual(["host-4", "host-3"]);
    expect(byId(targets, "host-3")).toMatchObject({ session_id: null, status: "pinned" });
  });

  it("用户明确 @ 了自动保留的那台：从此是用户固定的", () => {
    const kept = retainPrevious([], web, undefined, db);
    const { pinned, alreadyIn } = pinHost(kept, { hostId: 3, name: "web-1", username: "root" }, [db]);
    expect(alreadyIn).toBe(false);
    expect(pinned[0].retained).toBe(false);
  });

  it("固定目标被「当前」代表时 chip 不再单独显示", () => {
    const kept = retainPrevious([], web, undefined, db);
    expect(pinIsCurrent(kept[0], web)).toBe(true);
    expect(pinIsCurrent(kept[0], web2)).toBe(true);
    expect(pinIsCurrent(kept[0], db)).toBe(false);
    expect(pinKey(kept[0])).toBe("host-3");
    expect(pinKey({ hostId: null, name: "x", username: "", sessionId: "s-q" })).toBe("s-q");
  });
});


describe("对话里出现过的目标", () => {
  it("登记最新的主机名（会话关了、切走后旧卡片仍认得）", () => {
    const known = new Map<string, KnownTarget>();
    registerTargets(known, buildScopeTargets([web, db], [{ hostId: 9, name: "cache-1", username: "ops" }]));
    expect(known.get("host-3")?.label).toBe("web-1");
    expect(known.get("host-9")).toMatchObject({ label: "cache-1", hostId: 9, sessionId: null });
    registerTargets(known, buildScopeTargets([], []));
    expect(known.get("host-3")?.label).toBe("web-1");
  });
});
