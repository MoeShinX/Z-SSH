import { describe, expect, it } from "vitest";

import type { SftpTransfer } from "../../api/sftp";
import { reconcileSnapshot, upsertTransfer } from "./SftpTabView";

const job = (id: number, done = 0) =>
  ({ id, state: "running", done_bytes: done, total_bytes: 100 }) as unknown as SftpTransfer;

describe("传输队列的更新", () => {
  it("已有任务原地替换，其余任务对象不变（行组件可以跳过重新渲染）", () => {
    const a = job(1);
    const b = job(2);
    const c = job(3);
    const next = upsertTransfer([a, b, c], job(2, 50));
    expect(next.map((t) => t.done_bytes)).toEqual([0, 50, 0]);
    expect(next[0]).toBe(a);
    expect(next[2]).toBe(c);
  });

  it("新任务按 id 插到合适位置；同一个对象再来一次不产生新数组", () => {
    const list = [job(1), job(5)];
    expect(upsertTransfer(list, job(3)).map((t) => t.id)).toEqual([1, 3, 5]);
    expect(upsertTransfer(list, job(9)).map((t) => t.id)).toEqual([1, 5, 9]);
    expect(upsertTransfer(list, list[0])).toBe(list);
  });
});

describe("快照和事件合并", () => {
  const t = (id: number, state: string, done: number) =>
    ({ id, state, done_bytes: done, total_bytes: 100 }) as unknown as SftpTransfer;
  const all = (...ids: number[]) => new Set(ids);

  it("等快照期间完成的任务：不被快照里的传输中盖掉", () => {
    const merged = reconcileSnapshot([t(1, "done", 100)], [t(1, "running", 80)], all(1), 0);
    expect(merged).toEqual([t(1, "done", 100)]);
  });

  it("重试：事件是排队中 0 字节，旧快照是失败 500 字节，以事件为准", () => {
    const merged = reconcileSnapshot([t(1, "queued", 0)], [t(1, "failed", 50)], all(1), 0);
    expect(merged).toEqual([t(1, "queued", 0)]);
  });

  it("同一状态都在跑：快照字节更多就用快照（节流没发出来的进度）", () => {
    expect(reconcileSnapshot([t(1, "running", 40)], [t(1, "running", 60)], all(1), 0)[0].done_bytes).toBe(60);
    expect(reconcileSnapshot([t(1, "running", 60)], [t(1, "running", 40)], all(1), 0)[0].done_bytes).toBe(60);
  });

  it("挂载：只在快照里的补上，只在事件里的保留，按 id 排", () => {
    const merged = reconcileSnapshot([t(3, "done", 100)], [t(1, "done", 100), t(2, "queued", 0)], all(3), 0);
    expect(merged.map((x) => x.id)).toEqual([1, 2, 3]);
  });

  it("清除已完成：没收到事件的以快照为准（被清掉的消失）；刚被清的旧任务不复活；请求后新建的保留", () => {
    const current = [t(1, "done", 100), t(2, "done", 100), t(3, "done", 100), t(4, "done", 100)];
    // 1 被清掉、没事件；2 在等的期间完成了（快照里还是传输中）；3 等的期间收到过完成事件但被清掉了；
    // 4 是请求之后才建、又很快完成的新任务（快照里没有）
    const merged = reconcileSnapshot(current, [t(2, "running", 50)], all(2, 3, 4), 3);
    expect(merged).toEqual([t(2, "done", 100), t(4, "done", 100)]);
  });
});
