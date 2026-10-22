import { describe, expect, it } from "vitest";

import type { Group, Host } from "../api/vault";
import {
  filteredHostOrder,
  compareNames,
  groupKey,
  hostMatches,
  hostsUnder,
  parseExpanded,
  parseRecent,
  recordRecent,
  RECENT_HOSTS_MAX,
  serializeExpanded,
  sortByRecent,
  toggleExpanded,
  totalUnder,
  treeFilterFor,
  UNGROUPED_KEY,
} from "./hostTree";

const group = (id: number, name: string, parent_id: number | null = null): Group => ({
  id,
  name,
  parent_id,
  sort: 0,
});

const host = (id: number, name: string, group_id: number | null, tags: string[] = []): Host =>
  ({
    id,
    group_id,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags,
  }) as unknown as Host;

const groups = [group(1, "wap"), group(2, "Wap-PVE"), group(3, "HK", 2)];
const hosts = [
  host(1, "CA1 PVE", 2),
  host(2, "DE1 PVE", 2),
  host(3, "HK2 PVE", 3, ["生产"]),
  host(4, "loose", null),
];

describe("主机树", () => {
  it("展开状态存取，坏的键丢掉", () => {
    const expanded = parseExpanded("g2, ungrouped,bad,g,g10");
    expect([...expanded].sort()).toEqual(["g10", "g2", UNGROUPED_KEY]);
    expect(serializeExpanded(toggleExpanded(expanded, "g2"))).toBe("g10,ungrouped");
    expect(toggleExpanded(new Set(), groupKey(5)).has("g5")).toBe(true);
  });

  it("分组下面直接挂的主机按名字排，搜索时只留过滤结果里的", () => {
    expect(hostsUnder(hosts, 2, null).map((h) => h.name)).toEqual(["CA1 PVE", "DE1 PVE"]);
    expect(hostsUnder(hosts, 2, treeFilterFor(groups, hosts, "de1")).map((h) => h.name)).toEqual(["DE1 PVE"]);
    expect(hostsUnder(hosts, null, null).map((h) => h.name)).toEqual(["loose"]);
  });

  it("没在搜索（空白词）时没有过滤结果", () => {
    expect(treeFilterFor(groups, hosts, "")).toBeNull();
    expect(treeFilterFor(groups, hosts, "   ")).toBeNull();
  });

  it("主机匹配：显示它和它的各级上级分组，计数是过滤后的数量", () => {
    const filter = treeFilterFor(groups, hosts, "生产")!;
    expect([...filter.hostIds]).toEqual([3]);
    expect([...filter.groupIds].sort()).toEqual([2, 3]);
    expect(filter.ungrouped).toBe(false);
    expect(filter.counts.get(3)).toBe(1);
    expect(filter.counts.get(2)).toBe(1);
    expect(filter.total).toBe(1);
    const loose = treeFilterFor(groups, hosts, "LOOSE")!;
    expect(loose.ungrouped).toBe(true);
    expect([...loose.groupIds]).toEqual([]);
  });

  it("名称、地址、用户名、标签都能搜，不分大小写", () => {
    const ids = (text: string) => [...treeFilterFor(groups, hosts, text)!.hostIds].sort();
    expect(ids("ca1")).toEqual([1]);
    expect(ids("EXAMPLE.internal")).toEqual([1, 2, 3, 4]);
    expect(ids("ROOT")).toEqual([1, 2, 3, 4]);
    expect(ids("生产")).toEqual([3]);
    expect(ids("不存在")).toEqual([]);
  });

  it("分组名匹配：显示整个子树的全部主机，其他分组不显示", () => {
    const filter = treeFilterFor(groups, hosts, "wap-pve")!;
    expect([...filter.hostIds].sort()).toEqual([1, 2, 3]);
    expect([...filter.groupIds].sort()).toEqual([2, 3]);
    expect(filter.counts.get(2)).toBe(3);
    expect(filter.counts.get(3)).toBe(1);
    // 另一个分组 wap 的名字也含 "wap"，但它下面没有主机
    expect([...treeFilterFor(groups, hosts, "wap")!.groupIds].sort()).toEqual([1, 2, 3]);
  });

  it("「未分组」这个名字也能搜到，连带全部未分组主机", () => {
    const filter = treeFilterFor(groups, hosts, "未分")!;
    expect(filter.ungrouped).toBe(true);
    expect([...filter.hostIds]).toEqual([4]);
  });

  it("搜索结果的主机顺序和树里从上到下一致：先分组（子分组在前、主机在后）、最后未分组", () => {
    const filter = treeFilterFor(groups, hosts, "e")!;
    expect(filteredHostOrder(groups, hosts, filter).map((h) => h.name)).toEqual([
      "HK2 PVE",
      "CA1 PVE",
      "DE1 PVE",
      "loose",
    ]);
  });

  it("分组的数字包括子分组里的主机", () => {
    expect(totalUnder(groups, hosts, 2)).toBe(3);
    expect(totalUnder(groups, hosts, 3)).toBe(1);
    expect(totalUnder(groups, hosts, 1)).toBe(0);
  });

  it("按标签也能搜到", () => {
    expect(hostMatches(hosts[2], "生产")).toBe(true);
    expect(hostMatches(hosts[0], "生产")).toBe(false);
  });
});

describe("按名字排序", () => {
  it("数字 → 英文（不分大小写）→ 中文（按拼音）→ 其他符号；数字按大小", () => {
    const names = [
      "诛仙",
      "[备用]",
      "Bero-De",
      "andnode-40M",
      "极无双",
      "10.0.0.1",
      "口袋妖怪",
      "beecloud",
      "2-test",
      "_tmp",
      "上海电信1G",
      "NETCUP",
    ];
    expect([...names].sort(compareNames)).toEqual([
      "2-test",
      "10.0.0.1",
      "andnode-40M",
      "beecloud",
      "Bero-De",
      "NETCUP",
      "极无双",
      "口袋妖怪",
      "上海电信1G",
      "诛仙",
      "_tmp",
      "[备用]",
    ]);
  });
});

describe("主机页按最近打开排", () => {
  const named = (id: number, name: string) => ({ id, name }) as Host;

  it("打开的挪到最前，重复的不重复记，超出上限丢最早的", () => {
    expect(recordRecent("", 5)).toBe("5");
    expect(recordRecent("5,3", 3)).toBe("3,5");
    expect(parseRecent("3, 5,abc,,-1")).toEqual([3, 5]);
    const full = Array.from({ length: RECENT_HOSTS_MAX }, (_, i) => i + 1).join(",");
    const next = parseRecent(recordRecent(full, 99));
    expect(next[0]).toBe(99);
    expect(next).toHaveLength(RECENT_HOSTS_MAX);
    expect(next).not.toContain(RECENT_HOSTS_MAX);
  });

  it("最近打开的在前（越近越前），其余按名字；记录里已删掉的主机不碍事", () => {
    const hosts = [named(1, "诛仙"), named(2, "beecloud"), named(3, "192.0.2.1"), named(4, "andnode"), named(5, "极无双")];
    expect(sortByRecent(hosts, [5, 42, 2]).map((host) => host.name)).toEqual([
      "极无双",
      "beecloud",
      "192.0.2.1",
      "andnode",
      "诛仙",
    ]);
  });
});
