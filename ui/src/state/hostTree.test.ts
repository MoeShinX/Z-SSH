import { describe, expect, it } from "vitest";

import type { Group, Host } from "../api/vault";
import {
  autoExpandedFor,
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

  it("分组下面直接挂的主机按名字排，搜索时只留匹配的", () => {
    expect(hostsUnder(hosts, 2, "").map((h) => h.name)).toEqual(["CA1 PVE", "DE1 PVE"]);
    expect(hostsUnder(hosts, 2, "de1").map((h) => h.name)).toEqual(["DE1 PVE"]);
    expect(hostsUnder(hosts, null, "").map((h) => h.name)).toEqual(["loose"]);
  });

  it("搜索时自动展开有匹配主机的分组和它的上级", () => {
    expect([...autoExpandedFor(groups, hosts, "生产")].sort()).toEqual(["g2", "g3"]);
    expect([...autoExpandedFor(groups, hosts, "loose")]).toEqual([UNGROUPED_KEY]);
    expect(autoExpandedFor(groups, hosts, "").size).toBe(0);
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
