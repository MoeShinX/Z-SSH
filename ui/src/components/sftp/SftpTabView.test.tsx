//! 队列各状态显示的单测（M3-2 第 6.1 节）。

import { describe, expect, it } from "vitest";

import type { SftpTransfer } from "../../api/sftp";
import { transferIsActive } from "./SftpTabView";

const base: SftpTransfer = {
  id: 1,
  direction: "upload",
  state: "running",
  source: "C:\\a",
  target: "/tmp/a",
  total_files: 3,
  done_files: 1,
  total_bytes: 300,
  done_bytes: 100,
  speed_bps: 1024,
  notes: [],
};

describe("队列各状态的显示", () => {
  it("活跃（未完成）：排队、准备、传输中、暂停、等待重连都算", () => {
    for (const state of ["queued", "planning", "running", "paused", "waiting_reconnect"] as const) {
      expect(transferIsActive({ ...base, state })).toBe(true);
    }
  });

  it("终态：完成、失败、取消、跳过都不算", () => {
    for (const state of ["done", "failed", "cancelled", "skipped"] as const) {
      expect(transferIsActive({ ...base, state })).toBe(false);
    }
  });

  it("关标签 / 退出时的确认只数活跃的（M3-2 第 3 节）", () => {
    const transfers: SftpTransfer[] = [
      { ...base, state: "running", id: 1 },
      { ...base, state: "done", id: 2 },
      { ...base, state: "failed", id: 3 },
      { ...base, state: "paused", id: 4 },
    ];
    expect(transfers.filter(transferIsActive).length).toBe(2);
  });
});
