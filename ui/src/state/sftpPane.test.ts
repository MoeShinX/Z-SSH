//! SFTP 两栏共用逻辑的单测（M3-2 第 6.1 节）：虚拟列表（20000 条）、排序和过滤、
//! 多选和键盘操作。

import { describe, expect, it } from "vitest";

import type { SftpEntry } from "../api/sftp";
import {
  collapseCrumbs,
  editUploadNeedsRefresh,
  formatSize,
  localCrumbs,
  localParent,
  queueSummary,
  sanitizeSplitRatio,
  splitRatioFromPointer,
  remoteCrumbs,
  remoteParent,
  remoteParentDir,
  selectAll,
  selectionAfterArrow,
  selectionAfterClick,
  sortEntries,
  validateName,
  virtualWindow,
  visibleEntries,
  ROW_HEIGHT,
  type SortOrder,
  filterSorted,
  indicesOf,
  pathsOf,
  sortForDisplay,
} from "./sftpPane";

function entry(name: string, patch: Partial<SftpEntry> = {}): SftpEntry {
  return {
    name,
    path: `/${name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    ...patch,
  };
}

/** 造 N 条数据（性能和边界都要吃得住 20000 条）。 */
function manyEntries(count: number): SftpEntry[] {
  return Array.from({ length: count }, (_, index) =>
    entry(`file-${String(index).padStart(5, "0")}.txt`, {
      size: index * 17,
      mtime: 1_700_000_000 + index,
    }),
  );
}

describe("虚拟滚动（20000 条不卡的关键）", () => {
  it("只渲染可视区附近的一段", () => {
    const rows = 20_000;
    const win = virtualWindow(0, 600, rows);
    expect(win.start).toBe(0);
    expect(win.end).toBeLessThanOrEqual(40); // 600/26 ≈ 24 行 + overscan
    expect(win.totalHeight).toBe(rows * ROW_HEIGHT);
    expect(win.offsetY).toBe(0);
  });

  it("滚到中间：窗口跟着走，偏移正好是起始行的高度", () => {
    const rows = 20_000;
    const scrollTop = 500 * ROW_HEIGHT;
    const win = virtualWindow(scrollTop, 600, rows);
    expect(win.start).toBe(500 - 8); // overscan 8
    expect(win.offsetY).toBe(win.start * ROW_HEIGHT);
    // 渲染段盖住了可视区
    expect(win.start * ROW_HEIGHT).toBeLessThanOrEqual(scrollTop);
    expect(win.end * ROW_HEIGHT).toBeGreaterThanOrEqual(scrollTop + 600);
  });

  it("滚到底：不再越界", () => {
    const rows = 100;
    const win = virtualWindow(1_000_000, 600, rows);
    expect(win.end).toBe(rows);
    expect(win.totalHeight).toBe(rows * ROW_HEIGHT);
  });

  it("20000 条的窗口计算是瞬时的（回归护栏）", () => {
    const rows = manyEntries(20_000).length;
    const started = performance.now();
    for (let i = 0; i < 10_000; i += 1) {
      virtualWindow((i % rows) * ROW_HEIGHT, 800, rows);
    }
    // 1 万次窗口计算 100ms 内（保守门槛：真卡的时候这里会翻几十倍）。
    expect(performance.now() - started).toBeLessThan(100);
  });
});

describe("排序和过滤", () => {
  const entries: SftpEntry[] = [
    entry("b.txt", { size: 20, mtime: 200, kind: "file" }),
    entry("a", { kind: "dir" }),
    entry("c.txt", { size: 10, mtime: 300, kind: "file" }),
    entry("d.txt", { size: 30, mtime: 100, kind: "file", hidden: true }),
  ];

  it("目录永远排在文件前面（不管按什么排）", () => {
    for (const key of ["name", "size", "mtime", "kind"] as const) {
      const order: SortOrder = { key, descending: false };
      expect(sortEntries(entries, order)[0].name).toBe("a");
    }
  });

  it("按名字排序升序 / 降序", () => {
    const asc = sortEntries(entries, { key: "name", descending: false });
    expect(asc.map((item) => item.name)).toEqual(["a", "b.txt", "c.txt", "d.txt"]);
    const desc = sortEntries(entries, { key: "name", descending: true });
    expect(desc.map((item) => item.name)).toEqual(["a", "d.txt", "c.txt", "b.txt"]);
  });

  it("按大小和修改时间排序", () => {
    const bySize = sortEntries(entries, { key: "size", descending: false });
    expect(bySize.map((item) => item.name)).toEqual(["a", "c.txt", "b.txt", "d.txt"]);
    const byMtime = sortEntries(entries, { key: "mtime", descending: true });
    expect(byMtime.map((item) => item.name)).toEqual(["a", "c.txt", "b.txt", "d.txt"]);
  });

  it("过滤：隐藏文件默认不显示，开关打开才显示", () => {
    const hidden = visibleEntries(entries, { text: "", showHidden: false }, { key: "name", descending: false });
    expect(hidden.map((item) => item.name)).not.toContain("d.txt");
    const shown = visibleEntries(entries, { text: "", showHidden: true }, { key: "name", descending: false });
    expect(shown.map((item) => item.name)).toContain("d.txt");
  });

  it("过滤：按名字的子串（大小写不敏感）", () => {
    const kept = visibleEntries(
      entries,
      { text: "B.T", showHidden: false },
      { key: "name", descending: false },
    );
    expect(kept.map((item) => item.name)).toEqual(["b.txt"]);
  });

  it("过滤 + 排序一起：先藏再排", () => {
    const kept = visibleEntries(
      entries,
      { text: "txt", showHidden: false },
      { key: "mtime", descending: false },
    );
    expect(kept.map((item) => item.name)).toEqual(["b.txt", "c.txt"]);
  });

  it("数字名字按数值排（file-2 < file-10）", () => {
    const numbered = [entry("file-10.txt"), entry("file-2.txt")];
    const sorted = sortEntries(numbered, { key: "name", descending: false });
    expect(sorted.map((item) => item.name)).toEqual(["file-2.txt", "file-10.txt"]);
  });
});

describe("多选和键盘操作", () => {
  it("裸点只选一个", () => {
    const result = selectionAfterClick(3, null, new Set([1]), { ctrl: false, shift: false }, 10);
    expect([...result.selection]).toEqual([3]);
    expect(result.anchor).toBe(3);
  });

  it("Ctrl+点：加 / 减", () => {
    let result = selectionAfterClick(3, 1, new Set([1]), { ctrl: true, shift: false }, 10);
    expect([...result.selection]).toEqual([1, 3]);
    result = selectionAfterClick(3, 3, new Set([1, 3]), { ctrl: true, shift: false }, 10);
    expect([...result.selection]).toEqual([1]);
  });

  it("Shift+点：从锚点到点的整段", () => {
    const result = selectionAfterClick(7, 2, new Set([2]), { ctrl: false, shift: true }, 10);
    expect([...result.selection]).toEqual([2, 3, 4, 5, 6, 7]);
    expect(result.anchor).toBe(2);
  });

  it("Shift+点反方向也一样", () => {
    const result = selectionAfterClick(1, 5, new Set([5]), { ctrl: false, shift: true }, 10);
    expect([...result.selection]).toEqual([1, 2, 3, 4, 5]);
  });

  it("Ctrl+A 全选", () => {
    expect(selectAll(5).size).toBe(5);
    expect(selectAll(0).size).toBe(0);
  });

  it("方向键移动：不带 Shift 是单选，带 Shift 扩展", () => {
    const plain = selectionAfterArrow(2, 2, false, 1, 10);
    expect([...plain.selection]).toEqual([3]);
    expect(plain.focused).toBe(3);

    const extend = selectionAfterArrow(3, 2, true, 1, 10);
    expect([...extend.selection]).toEqual([2, 3, 4]);
    expect(extend.anchor).toBe(2);
  });

  it("方向键在边界上不越界", () => {
    const top = selectionAfterArrow(0, 0, false, -1, 10);
    expect(top.focused).toBe(0);
    const bottom = selectionAfterArrow(9, 9, false, 1, 10);
    expect(bottom.focused).toBe(9);
    const empty = selectionAfterArrow(0, null, true, 1, 0);
    expect(empty.selection.size).toBe(0);
  });
});

describe("面包屑和路径", () => {
  it("远端面包屑", () => {
    expect(remoteCrumbs("/")).toEqual([]);
    expect(remoteCrumbs("/tmp/a/b")).toEqual([
      { name: "tmp", path: "/tmp" },
      { name: "a", path: "/tmp/a" },
      { name: "b", path: "/tmp/a/b" },
    ]);
  });

  it("远端上一级（根没有上级）", () => {
    expect(remoteParent("/tmp/a")).toBe("/tmp");
    expect(remoteParent("/tmp")).toBe("/");
    expect(remoteParent("/")).toBe("/");
  });

  it("本地面包屑（含盘符）和上一级（此电脑）", () => {
    expect(localCrumbs("")).toEqual([]);
    const crumbs = localCrumbs("C:\\Users\\张 三");
    expect(crumbs.map((crumb) => crumb.name)).toEqual(["C:", "Users", "张 三"]);
    expect(crumbs[0].path).toBe("C:\\");
    expect(crumbs[2].path).toBe("C:\\Users\\张 三");
    expect(localParent("C:\\a\\b")).toBe("C:\\a");
    expect(localParent("C:\\")).toBe("");
  });

  it("网络共享：服务器和共享名是一个根，不能走出共享根（R7 C11）", () => {
    const root = "\\\\nas\\public\\";
    const crumbs = localCrumbs("\\\\nas\\public\\docs\\2026");
    expect(crumbs.map((crumb) => crumb.name)).toEqual(["\\\\nas\\public", "docs", "2026"]);
    expect(crumbs.map((crumb) => crumb.path)).toEqual([
      root,
      "\\\\nas\\public\\docs",
      "\\\\nas\\public\\docs\\2026",
    ]);
    expect(localParent("\\\\nas\\public\\docs\\2026")).toBe("\\\\nas\\public\\docs");
    expect(localParent("\\\\nas\\public\\docs")).toBe(root);
    expect(localParent(root)).toBe("");
    expect(localParent("\\\\nas\\public")).toBe("");
  });

  it("带长路径前缀的也按盘符 / 共享解析", () => {
    expect(localCrumbs("\\\\?\\UNC\\nas\\public\\docs").map((crumb) => crumb.path)).toEqual([
      "\\\\nas\\public\\",
      "\\\\nas\\public\\docs",
    ]);
    expect(localCrumbs("\\\\?\\D:\\data").map((crumb) => crumb.path)).toEqual(["D:\\", "D:\\data"]);
  });
});

describe("名字校验（新建 / 重命名）", () => {
  it("空名字不行", () => {
    expect(validateName("  ", true).ok).toBe(false);
    expect(validateName("", false).ok).toBe(false);
  });

  it("远端禁 /，本地还要禁 Windows 非法字符", () => {
    expect(validateName("a/b", true).ok).toBe(false);
    expect(validateName("a\\b", true).ok).toBe(true);
    expect(validateName("a\\b", false).ok).toBe(false);
    for (const bad of ["a:b", "a*b", "a?b", 'a"b', "a<b", "a>b", "a|b"]) {
      expect(validateName(bad, false).ok).toBe(false);
    }
    expect(validateName("正常名字.txt", false).ok).toBe(true);
  });
});

describe("显示辅助", () => {
  it("大小", () => {
    expect(formatSize(undefined)).toBe("—");
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});

describe("停靠面板：上下分隔线比例", () => {
  it("鼠标位置换算成远端栏占的份额，并夹在 [0.15, 0.85]", () => {
    expect(splitRatioFromPointer(300, 100, 400)).toBeCloseTo(0.5);
    expect(splitRatioFromPointer(100, 100, 400)).toBe(0.15);
    expect(splitRatioFromPointer(10_000, 100, 400)).toBe(0.85);
    expect(splitRatioFromPointer(300, 100, 0)).toBe(0.5);
  });

  it("读回来的比例：不是数字、越界都回到默认的平分", () => {
    expect(sanitizeSplitRatio(0.3)).toBe(0.3);
    expect(sanitizeSplitRatio(0.02)).toBe(0.5);
    expect(sanitizeSplitRatio(1.5)).toBe(0.5);
    expect(sanitizeSplitRatio("0.3")).toBe(0.5);
    expect(sanitizeSplitRatio(Number.NaN)).toBe(0.5);
    expect(sanitizeSplitRatio(null)).toBe(0.5);
  });
});

describe("停靠面板：传输队列摘要", () => {
  const item = (state: string, done: number, total: number) => ({
    state,
    done_bytes: done,
    total_bytes: total,
  });

  it("只数没结束的任务；百分比按字节合计算", () => {
    const summary = queueSummary([item("running", 30, 100), item("queued", 0, 100), item("done", 50, 50)]);
    expect(summary).toEqual({ active: 2, percent: 15, text: "2 个任务进行中 15%" });
  });

  it("没有进行中的任务：没有摘要；总字节还不知道（0）时按 0%", () => {
    expect(queueSummary([item("done", 5, 5), item("failed", 1, 9)])).toBeNull();
    expect(queueSummary([])).toBeNull();
    expect(queueSummary([item("planning", 0, 0)])?.text).toBe("1 个任务进行中 0%");
  });
});

describe("窄面板的路径面包屑（M3-4 第 0 节）", () => {
  const crumbs = remoteCrumbs("/home/deploy/releases/2026-10-17/app/current");

  it("放得下就一级不折", () => {
    expect(collapseCrumbs(crumbs, 2000)).toEqual({ hidden: [], shown: crumbs });
  });

  it("放不下时保留最后几级，前面的折叠，且顺序不乱", () => {
    const result = collapseCrumbs(crumbs, 160);
    expect(result.hidden.length).toBeGreaterThan(0);
    expect([...result.hidden, ...result.shown]).toEqual(crumbs);
    expect(result.shown[result.shown.length - 1].name).toBe("current");
    expect(result.shown.length).toBeLessThan(crumbs.length);
  });

  it("窄到极限也至少留最后一级", () => {
    const result = collapseCrumbs(crumbs, 10);
    expect(result.shown.map((c) => c.name)).toEqual(["current"]);
    expect(result.hidden.length).toBe(crumbs.length - 1);
  });

  it("还没量到宽度（0）时不折", () => {
    expect(collapseCrumbs(crumbs, 0).hidden).toEqual([]);
  });

  it("本地路径同样适用", () => {
    const local = localCrumbs(String.raw`C:\zssh-shots\m33\data\downloads`);
    const result = collapseCrumbs(local, 150);
    expect(result.shown[result.shown.length - 1].name).toBe("downloads");
  });
});

describe("编辑器保存完成后刷新远端栏", () => {
  const event = { sftp_id: "s1", state: "uploaded", remote_path: "/tmp/a/edit-me.txt" };

  it("同一会话、文件在当前目录里 → 刷新（含另存为的新文件名）", () => {
    expect(editUploadNeedsRefresh(event, "s1", "/tmp/a")).toBe(true);
    expect(
      editUploadNeedsRefresh({ ...event, remote_path: "/tmp/a/edit-me (副本).txt" }, "s1", "/tmp/a/"),
    ).toBe(true);
  });

  it("别的会话、别的目录、别的状态都不刷新", () => {
    expect(editUploadNeedsRefresh(event, "s2", "/tmp/a")).toBe(false);
    expect(editUploadNeedsRefresh(event, "s1", "/tmp/b")).toBe(false);
    expect(editUploadNeedsRefresh({ ...event, state: "conflict" }, "s1", "/tmp/a")).toBe(false);
    expect(editUploadNeedsRefresh(event, "s1", "")).toBe(false);
  });

  it("根目录下的文件", () => {
    expect(remoteParentDir("/a.txt")).toBe("/");
    expect(editUploadNeedsRefresh({ ...event, remote_path: "/a.txt" }, "s1", "/")).toBe(true);
  });
});

describe("排序和过滤拆开", () => {
  const e = (name: string, hidden = false) =>
    ({ name, path: `/d/${name}`, kind: "file", name_is_utf8: true, hidden }) as never;

  it("先排序再过滤，和原来先过滤再排序结果一样", () => {
    const entries = [e("file10"), e("File2"), e(".hidden", true), e("文件"), e("file1")];
    const order = { key: "name" as const, descending: false };
    for (const filter of [
      { text: "", showHidden: false },
      { text: "FILE", showHidden: true },
      { text: "1", showHidden: false },
    ]) {
      const sorted = sortForDisplay(entries, order);
      expect(filterSorted(sorted, filter)).toEqual(visibleEntries(entries, filter, order));
    }
    // 数字按数值排：file2 在 file10 前面
    const names: string[] = sortForDisplay(entries, order).entries.map((x: { name: string }) => x.name);
    expect(names.filter((n) => /^file\d+$/i.test(n))).toEqual(["file1", "File2", "file10"]);
  });

  it("路径集合和行号来回换，当前列表里没有的不算", () => {
    const rows = [e("a"), e("b"), e("c")];
    expect([...indicesOf(rows, new Set(["/d/c", "/d/gone"]))]).toEqual([2]);
    expect([...pathsOf(rows, [0, 2, 9])]).toEqual(["/d/a", "/d/c"]);
  });

  it("10 万条：排序一次之后，过滤不再重排", () => {
    const big = Array.from({ length: 100_000 }, (_, i) => e(`file-${(i * 7919) % 100_000}.txt`));
    const order = { key: "name" as const, descending: false };
    const t0 = performance.now();
    const sorted = sortForDisplay(big, order);
    const sortMs = performance.now() - t0;
    const t1 = performance.now();
    for (const text of ["f", "fi", "fil", "file-9", "file-99"]) filterSorted(sorted, { text, showHidden: false });
    const filterMs = (performance.now() - t1) / 5;
    // 只是防回退的粗略上限（不是性能测试）：过滤要比排序快一个量级
    expect(filterMs).toBeLessThan(sortMs);
  });
});
