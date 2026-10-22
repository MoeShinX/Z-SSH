//! AI 目标列表（AI-SCOPE-1）：按主机去重、切焦点保留、调用目标解析。

import { describe, expect, it } from "vitest";

import {
  buildScopeTargets,
  describeCallTarget,
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
const db: ScopeSession = { sessionId: "s-db", hostId: 4, hostLabel: "db-1", username: "root" };
const quick: ScopeSession = { sessionId: "s-q", hostId: null, hostLabel: "临时", username: "u" };

const ids = (list: { target_id: string }[]) => list.map((t) => t.target_id);

describe("按主机去重", () => {
  it("没有 @：只有当前标签的会话，第一个是当前", () => {
    const targets = buildScopeTargets([web, db], []);
    expect(ids(targets)).toEqual(["s-web", "s-db"]);
    expect(targets.map((t) => t.current)).toEqual([true, false]);
    expect(targets.every((t) => !t.pinned)).toBe(true);
  });

  it("当前会话的主机又被 @：只发一个目标，且标 pinned", () => {
    const { pinned, alreadyIn } = pinHost(
      [],
      { hostId: 3, name: "web-1", username: "root" },
      [web, db],
    );
    expect(alreadyIn).toBe(true);
    const targets = buildScopeTargets([web, db], pinned);
    expect(ids(targets)).toEqual(["s-web", "s-db"]);
    expect(targets[0]).toMatchObject({ target_id: "s-web", current: true, pinned: true });
    expect(targets[1].pinned).toBe(false);
  });

  it("@ 一台没打开的主机：host-<id> 目标", () => {
    const { pinned, alreadyIn } = pinHost(
      [],
      { hostId: 9, name: "cache-1", username: "ops" },
      [web],
    );
    expect(alreadyIn).toBe(false);
    const targets = buildScopeTargets([web], pinned);
    expect(ids(targets)).toEqual(["s-web", "host-9"]);
    expect(targets[1]).toMatchObject({ pinned: true, current: false, status: "pinned" });
  });

  it("切到那台主机的标签后合并成一个：目标 ID 不变（host-<id>），当前标记转给它", () => {
    const pinned: PinnedHost[] = [{ hostId: 9, name: "cache-1", username: "ops" }];
    const cache: ScopeSession = { sessionId: "s-cache", hostId: 9, hostLabel: "cache-1", username: "ops" };
    const before = buildScopeTargets([web], pinned);
    const after = buildScopeTargets([cache], pinned);
    expect(ids(after)).toEqual(["host-9"]);
    expect(after[0]).toMatchObject({ current: true, pinned: true });
    // 前后 host-9 都在：同步给后端时不会被移出再加回来
    expect(ids(before)).toContain("host-9");
  });

  it("同一台主机重复 @ 不会出现第二个目标", () => {
    const once = pinHost([], { hostId: 9, name: "cache-1", username: "ops" }, []);
    const twice = pinHost(once.pinned, { hostId: 9, name: "cache-1", username: "ops" }, []);
    expect(twice.pinned).toBe(once.pinned);
    expect(twice.alreadyIn).toBe(true);
  });

  it("两个窗格都是同一台主机又 @ 了它（没绑会话）：只留 host-<id>", () => {
    const a: ScopeSession = { ...web, sessionId: "s-a" };
    const b: ScopeSession = { ...web, sessionId: "s-b" };
    const targets = buildScopeTargets([a, b], [{ hostId: 3, name: "web-1", username: "root" }]);
    expect(ids(targets)).toEqual(["host-3"]);
    expect(targets[0].current).toBe(true);
  });
});

describe("切焦点保留（对话进行中）", () => {
  it("原来的当前转成固定目标：会话 ID 不变，仍在范围里，新焦点成为当前", () => {
    const kept = retainPrevious([], web);
    expect(kept).toHaveLength(1);
    expect(kept[0]).toMatchObject({ sessionId: "s-web", retained: true });
    const targets = buildScopeTargets([db], kept);
    expect(ids(targets)).toEqual(["s-db", "s-web"]);
    expect(targets[0]).toMatchObject({ current: true, pinned: false });
    expect(targets[1]).toMatchObject({ current: false, pinned: true });
  });

  it("切回来后合并成一个，仍是同一个目标 ID", () => {
    const kept = retainPrevious([], web);
    const back = buildScopeTargets([web], kept);
    expect(ids(back)).toEqual(["s-web"]);
    expect(back[0]).toMatchObject({ current: true, pinned: true });
  });

  it("已经被 @ 代表的不重复保留", () => {
    const pinnedByHost: PinnedHost[] = [{ hostId: 3, name: "web-1", username: "root" }];
    expect(retainPrevious(pinnedByHost, web)).toBe(pinnedByHost);
    const bound: PinnedHost[] = [
      { hostId: 3, name: "web-1", username: "root", sessionId: "s-web" },
    ];
    expect(retainPrevious(bound, web)).toBe(bound);
  });

  it("会话已经关了就不保留；没有上一个也不保留", () => {
    expect(retainPrevious([], web, new Set(["s-db"]))).toEqual([]);
    expect(retainPrevious([], null)).toEqual([]);
  });

  it("快速连接的会话（没有主机 id）也能保留", () => {
    const kept = retainPrevious([], quick);
    const targets = buildScopeTargets([db], kept);
    expect(ids(targets)).toEqual(["s-db", "s-q"]);
    expect(targets[1].pinned).toBe(true);
  });

  it("保留的会话关了：目标跟着消失，标签也清掉", () => {
    const kept = retainPrevious([], web);
    const open = new Set(["s-db"]);
    expect(ids(buildScopeTargets([db], kept, open))).toEqual(["s-db"]);
    expect(pruneRetained(kept, open)).toEqual([]);
    // 还开着就不动
    expect(pruneRetained(kept, new Set(["s-web", "s-db"]))).toBe(kept);
  });

  it("用户 @ 并绑在会话上的，会话关了退回按主机（host-<id>）", () => {
    const { pinned } = pinHost([], { hostId: 3, name: "web-1", username: "root" }, [web]);
    expect(pinned[0].sessionId).toBe("s-web");
    const targets = buildScopeTargets([db], pinned, new Set(["s-db"]));
    expect(ids(targets)).toEqual(["s-db", "host-3"]);
  });

  it("用户明确 @ 了自动保留的那台：从此是用户固定的", () => {
    const kept = retainPrevious([], web);
    const { pinned, alreadyIn } = pinHost(kept, { hostId: 3, name: "web-1", username: "root" }, [db]);
    expect(alreadyIn).toBe(false);
    expect(pinned[0].retained).toBe(false);
  });

  it("固定目标被「当前」代表时 chip 不再单独显示", () => {
    const kept = retainPrevious([], web);
    expect(pinIsCurrent(kept[0], web)).toBe(true);
    expect(pinIsCurrent(kept[0], db)).toBe(false);
    expect(pinKey(kept[0])).toBe("s-web");
    expect(pinKey({ hostId: 9, name: "x", username: "" })).toBe("host-9");
  });
});

describe("调用目标解析", () => {
  const known = new Map<string, KnownTarget>();
  registerTargets(known, [web, db], [{ hostId: 9, name: "cache-1", username: "ops" }]);

  it("显式 target_id 用它，省略时用当时的当前", () => {
    expect(describeCallTarget("zssh_run_command", { target_id: "s-db" }, known, "s-web", 2)).toMatchObject({
      id: "s-db",
      label: "db-1",
      hostId: 4,
      sessionId: "s-db",
      multi: true,
    });
    expect(describeCallTarget("zssh_run_command", { command: "ls" }, known, "s-web", 1)).toMatchObject({
      label: "web-1",
      multi: false,
    });
  });

  it("host-<id> 目标：主机 id 可用，没有会话 ID", () => {
    expect(
      describeCallTarget("zssh_read_terminal", { target_id: "host-9" }, known, null, 3),
    ).toMatchObject({ label: "cache-1", hostId: 9, sessionId: null });
  });

  it("列表类工具、不认识的目标没有目标", () => {
    expect(describeCallTarget("zssh_list_targets", {}, known, "s-web", 2)).toBeNull();
    expect(describeCallTarget("zssh_run_command", { target_id: "s-zzz" }, known, "s-web", 2)).toBeNull();
    expect(describeCallTarget("zssh_run_command", {}, known, null, 0)).toBeNull();
  });

  it("文件工具也有目标", () => {
    expect(describeCallTarget("zssh_read_file", { target_id: "s-web", path: "/etc/hosts" }, known, null, 2)).not.toBeNull();
  });
});
