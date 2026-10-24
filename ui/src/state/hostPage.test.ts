//! 主机页右边的显示选项（UI-16）：疏密 / 排序方式的解析，和三种排序（含选了分组时的范围）。

import { describe, expect, it } from "vitest";

import type { Group, Host } from "../api/vault";
import {
  groupWithDescendants,
  nextSortMode,
  pageHosts,
  parseDensity,
  parseSortMode,
  type VaultSortMode,
} from "./hostPage";

const group = (id: number, name: string, parent_id: number | null = null, sort = 0): Group => ({
  id,
  name,
  parent_id,
  sort,
});
const host = (id: number, name: string, group_id: number | null, sort = 0, tags: string[] = []) =>
  ({
    id,
    group_id,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags,
    sort,
  }) as unknown as Host;

// Alpha(1)：a1 a2 a3 + 子分组 Sub(3)：s1；Beta(2)：b1 b2；顶层：u1 u2
const groups = [group(1, "Alpha"), group(2, "Beta"), group(3, "Sub", 1)];
const hosts = [
  host(1, "a1", 1),
  host(2, "a2", 1),
  host(3, "a3", 1),
  host(4, "b1", 2),
  host(5, "b2", 2),
  host(6, "u1", null),
  host(7, "u2", null),
  host(8, "s1", 3),
];

const names = (list: Host[]) => list.map((item) => item.name);
const run = (
  mode: VaultSortMode,
  extra: { query?: string; selectedGroup?: number | null | "all"; recent?: number[]; groups?: Group[]; hosts?: Host[] } = {},
) =>
  names(
    pageHosts(extra.groups ?? groups, extra.hosts ?? hosts, {
      query: extra.query ?? "",
      selectedGroup: "selectedGroup" in extra ? (extra.selectedGroup as number | null | "all") : "all",
      mode,
      recent: extra.recent ?? [],
    }),
  );

describe("设置值解析", () => {
  it("疏密：只认 comfortable，其余（含没存过）都是紧凑", () => {
    expect(parseDensity("comfortable")).toBe("comfortable");
    expect(parseDensity("compact")).toBe("compact");
    expect(parseDensity("")).toBe("compact");
    expect(parseDensity(undefined)).toBe("compact");
    expect(parseDensity(3)).toBe("compact");
  });

  it("排序方式：读不懂的值按手动；点一下按 手动 → 最近打开 → 名称 → 手动 轮换", () => {
    expect(parseSortMode("recent")).toBe("recent");
    expect(parseSortMode("name")).toBe("name");
    expect(parseSortMode("manual")).toBe("manual");
    expect(parseSortMode("whatever")).toBe("manual");
    expect(parseSortMode(undefined)).toBe("manual");
    expect(nextSortMode("manual")).toBe("recent");
    expect(nextSortMode("recent")).toBe("name");
    expect(nextSortMode("name")).toBe("manual");
  });
});

describe("pageHosts", () => {
  it("manual：和左边的树一个顺序（子分组先、再本组主机，顶层主机在最后）", () => {
    expect(run("manual")).toEqual(["s1", "a1", "a2", "a3", "b1", "b2", "u1", "u2"]);
  });

  it("manual：每层按手动顺序（sort > 0 的在前），分组之间也按 sort", () => {
    const moved = [
      host(1, "a1", 1, 3),
      host(2, "a2", 1, 1),
      host(3, "a3", 1, 2),
      host(4, "b1", 2),
      host(5, "b2", 2),
      host(6, "u1", null),
      host(7, "u2", null, 1),
      host(8, "s1", 3),
    ];
    const order = [group(1, "Alpha", null, 2), group(2, "Beta", null, 1), group(3, "Sub", 1)];
    expect(run("manual", { groups: order, hosts: moved })).toEqual(["b1", "b2", "s1", "a2", "a3", "a1", "u2", "u1"]);
  });

  it("manual：选了分组只显示它和子分组里的主机，顺序按树", () => {
    expect(run("manual", { selectedGroup: 1 })).toEqual(["s1", "a1", "a2", "a3"]);
    expect(run("manual", { selectedGroup: 3 })).toEqual(["s1"]);
    expect(run("manual", { selectedGroup: 2 })).toEqual(["b1", "b2"]);
  });

  it("选「未分组」只看顶层主机", () => {
    expect(run("manual", { selectedGroup: null })).toEqual(["u1", "u2"]);
    expect(run("name", { selectedGroup: null })).toEqual(["u1", "u2"]);
  });

  it("recent：最近打开的在前（越近越前），其余按名字", () => {
    expect(run("recent", { recent: [7, 2] })).toEqual(["u2", "a2", "a1", "a3", "b1", "b2", "s1", "u1"]);
  });

  it("name：只按名字，不看手动顺序也不看最近打开", () => {
    const moved = hosts.map((item) => (item.id === 3 ? host(3, "a3", 1, 1) : item));
    expect(run("name", { hosts: moved, recent: [7] })).toEqual(["a1", "a2", "a3", "b1", "b2", "s1", "u1", "u2"]);
  });

  it("搜索：三种模式都先过滤，再按各自的顺序", () => {
    expect(run("manual", { query: "a1" })).toEqual(["a1"]);
    expect(run("manual", { query: "2" })).toEqual(["a2", "b2", "u2"]);
    expect(run("name", { query: "2" })).toEqual(["a2", "b2", "u2"]);
    expect(run("recent", { query: "2", recent: [7] })).toEqual(["u2", "a2", "b2"]);
  });

  it("搜索 + 选了分组：两个条件都要满足", () => {
    expect(run("manual", { query: "2", selectedGroup: 2 })).toEqual(["b2"]);
  });

  it("搜索也认标签", () => {
    const tagged = hosts.map((item) => (item.id === 5 ? host(5, "b2", 2, 0, ["prod"]) : item));
    expect(run("manual", { query: "prod", hosts: tagged })).toEqual(["b2"]);
  });

  it("什么都没有时是空的；分组有环也不会死循环", () => {
    expect(run("manual", { groups: [], hosts: [] })).toEqual([]);
    const loop = [group(1, "A", 2), group(2, "B", 1)];
    expect(groupWithDescendants(loop, 1)).toEqual(new Set([1, 2]));
    expect(run("manual", { groups: loop, hosts: [host(1, "x", 1)], selectedGroup: 1 })).toEqual(["x"]);
  });
});
