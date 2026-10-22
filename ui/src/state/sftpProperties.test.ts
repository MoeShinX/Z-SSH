import { describe, expect, it } from "vitest";
import type { SftpEntry, SftpSizeEvent } from "../api/sftp";
import {
  applySizeEvent,
  bytesText,
  calcText,
  canCalculate,
  countsText,
  IDLE_CALC,
  infoRows,
  propertiesTargets,
  sumTotals,
  type CalcState,
} from "./sftpProperties";

const entry = (name: string, patch: Partial<SftpEntry> = {}): SftpEntry => ({
  name,
  path: `/srv/${name}`,
  kind: "file",
  name_is_utf8: true,
  hidden: false,
  ...patch,
});

const event = (patch: Partial<SftpSizeEvent>): SftpSizeEvent => ({
  request_id: "req-1",
  calc_id: 1,
  state: "running",
  files: 0,
  dirs: 0,
  bytes: 0,
  links: 0,
  unreadable: 0,
  ...patch,
});

describe("propertiesTargets", () => {
  it("没选条目 = 当前目录自己；没有当前目录 = 没有对象", () => {
    expect(propertiesTargets([], "/srv/app")).toEqual([
      { name: "app", path: "/srv/app", kind: "dir" },
    ]);
    expect(propertiesTargets([], "C:\\work\\proj\\")[0].name).toBe("proj");
    expect(propertiesTargets([], "")).toEqual([]);
  });

  it("盘符不算对象", () => {
    expect(propertiesTargets([entry("C:", { kind: "drive" })], "")).toEqual([]);
  });
});

describe("显示", () => {
  it("远端文件：有权限和所有者；本地没有", () => {
    const [target] = propertiesTargets(
      [entry("a.conf", { size: 2048, mtime: 1_700_000_000, permissions: 0o100640, owner: "root" })],
      "/srv",
    );
    const remote = infoRows(target, "remote").map((row) => row.label);
    expect(remote).toEqual(["名称", "位置", "类型", "大小", "修改时间", "权限", "所有者"]);
    expect(infoRows(target, "remote").find((row) => row.label === "权限")?.value).toBe("0640");
    expect(infoRows(target, "local").map((row) => row.label)).not.toContain("权限");
  });

  it("文件夹不显示固定的大小行（要用户点计算）", () => {
    const [target] = propertiesTargets([entry("logs", { kind: "dir" })], "/srv");
    expect(infoRows(target, "remote").map((row) => row.label)).not.toContain("大小");
    expect(canCalculate(target)).toBe(true);
  });

  it("符号链接写出指向；链接本身不计算大小", () => {
    const [target] = propertiesTargets(
      [entry("cur", { kind: "symlink", link_target: "/srv/v2", target_kind: "dir" })],
      "/srv",
    );
    expect(infoRows(target, "remote").find((row) => row.label === "类型")?.value).toBe("符号链接 → /srv/v2");
    expect(canCalculate(target)).toBe(false);
  });

  it("字节数两种写法", () => {
    expect(bytesText(512)).toBe("512 字节");
    expect(bytesText(1_572_864)).toBe("1.5 MB（1,572,864 字节）");
  });

  it("文件夹数不把根目录自己算进去", () => {
    expect(countsText({ files: 3, dirs: 3, bytes: 0, links: 0, unreadable: 0 })).toBe("3 个文件，2 个文件夹");
    expect(countsText({ files: 0, dirs: 0, bytes: 0, links: 0, unreadable: 0 })).toBe("0 个文件，0 个文件夹");
  });
});

describe("计算大小的状态", () => {
  const running: CalcState = { phase: "running", requestId: "req-1", calcId: 1, totals: IDLE_CALC.totals };

  it("进度 → 完成", () => {
    const mid = applySizeEvent(running, event({ files: 10, dirs: 2, bytes: 100 }));
    expect(mid.phase).toBe("running");
    expect(calcText(mid)).toContain("计算中");
    const done = applySizeEvent(mid, event({ state: "done", files: 12, dirs: 3, bytes: 130 }));
    expect(done.phase).toBe("done");
    expect(calcText(done)).toBe("130 字节，12 个文件，2 个文件夹");
  });

  it("取消之后迟到的进度事件不会把状态改回进行中；取消的结果标明不完整", () => {
    const cancelled = applySizeEvent(running, event({ state: "cancelled", files: 5, bytes: 50 }));
    expect(applySizeEvent(cancelled, event({ files: 6 })).phase).toBe("cancelled");
    expect(calcText(cancelled)).toContain("不是完整结果");
  });

  it("别的任务号的事件忽略；完成后的事件也不再改", () => {
    expect(applySizeEvent(running, event({ calc_id: 9, state: "done" }))).toBe(running);
    const done = applySizeEvent(running, event({ state: "done", files: 1 }));
    expect(applySizeEvent(done, event({ files: 99 }))).toBe(done);
  });

  it("关联 ID 对不上的事件一律忽略（别的文件夹的进度不能串进来）", () => {
    // 还没拿到后端任务号的"进行中"状态只认自己发起时生成的关联 ID。
    const started: CalcState = { phase: "running", requestId: "req-a", totals: IDLE_CALC.totals };
    expect(applySizeEvent(started, event({ request_id: "req-b", files: 9, dirs: 9, bytes: 900 }))).toBe(
      started,
    );
    const mine = applySizeEvent(started, event({ request_id: "req-a", files: 2, bytes: 20 }));
    expect(calcText(mine)).toContain("2 个文件");
  });

  it("失败写出原因；跳过的链接和读不了的目录要说", () => {
    const failed = applySizeEvent(running, event({ state: "failed", error: "没有权限" }));
    expect(calcText(failed)).toBe("计算失败：没有权限");
    const done = applySizeEvent(running, event({ state: "done", files: 1, dirs: 1, links: 2, unreadable: 1 }));
    expect(calcText(done)).toContain("跳过 2 个符号链接");
    expect(calcText(done)).toContain("1 个文件夹读不了");
  });

  it("多个对象的合计", () => {
    const total = sumTotals([
      { files: 1, dirs: 1, bytes: 10, links: 0, unreadable: 0 },
      { files: 2, dirs: 2, bytes: 20, links: 1, unreadable: 1 },
    ]);
    expect(total).toEqual({ files: 3, dirs: 3, bytes: 30, links: 1, unreadable: 1 });
  });
});
