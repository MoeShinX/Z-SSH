/** @vitest-environment jsdom */
//! F06：界面显示的设置要和后端实际用的一致。
//!
//! 两条会让它们不一致的路径：设置写入失败时界面没回滚；同步 / 恢复备份把本地库改了，
//! 界面的缓存没刷新。

import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  settingsGet: vi.fn(async (_key: string): Promise<string | null> => null),
  settingsSet: vi.fn(async (_key: string, _value: string): Promise<void> => {}),
  toastError: vi.fn(),
  vaultLoad: vi.fn(async () => {}),
}));

vi.mock("../api/settings", () => ({
  settingsGet: mocks.settingsGet,
  settingsSet: mocks.settingsSet,
}));
vi.mock("../components/ui/toast", () => ({
  toast: {
    error: mocks.toastError,
    success: vi.fn(),
    info: vi.fn(),
    warning: vi.fn(),
  },
}));
vi.mock("../api/sync", () => ({
  onSyncStatus: vi.fn(async () => () => {}),
  syncStatus: vi.fn(async () => ({})),
}));
vi.mock("./vault", () => ({
  useVault: { getState: () => ({ load: mocks.vaultLoad }) },
}));

import type { SyncStatusView } from "../api/sync";
import { useSettings } from "./settings";
import { refreshLocalData, useSync } from "./sync";

function status(
  at: number | null,
  state: "completed" | "failed" = "completed",
): SyncStatusView {
  return {
    last: at === null ? null : { at, status: state },
  } as unknown as SyncStatusView;
}

beforeEach(() => {
  mocks.settingsGet.mockReset().mockResolvedValue(null);
  mocks.settingsSet.mockReset().mockResolvedValue(undefined);
  mocks.toastError.mockReset();
  mocks.vaultLoad.mockClear();
  useSettings.setState({ ai_exec_mode: "manual", terminal_font_size: 12 });
  useSync.setState({ status: null });
});

describe("设置写入失败", () => {
  it("界面回到原值并提示错误（执行模式是安全相关的设置）", async () => {
    mocks.settingsSet.mockRejectedValueOnce(new Error("数据库被占用"));
    const saved = await useSettings.getState().update("ai_exec_mode", "auto");
    expect(saved).toBe(false);
    expect(useSettings.getState().ai_exec_mode).toBe("manual");
    expect(mocks.toastError).toHaveBeenCalledTimes(1);
    expect(String(mocks.toastError.mock.calls[0][0])).toContain("数据库被占用");
  });

  it("其他设置也一样回滚", async () => {
    mocks.settingsSet.mockRejectedValueOnce("磁盘满了");
    expect(await useSettings.getState().update("terminal_font_size", 20)).toBe(
      false,
    );
    expect(useSettings.getState().terminal_font_size).toBe(12);
  });

  it("写入成功就保持新值", async () => {
    expect(await useSettings.getState().update("ai_exec_mode", "auto")).toBe(
      true,
    );
    expect(useSettings.getState().ai_exec_mode).toBe("auto");
    expect(mocks.settingsSet).toHaveBeenCalledWith(
      "ai_exec_mode",
      JSON.stringify("auto"),
    );
    expect(mocks.toastError).not.toHaveBeenCalled();
  });

  it("失败回滚时，期间别处又改过的值不会被盖掉", async () => {
    let reject: (reason: unknown) => void = () => {};
    mocks.settingsSet.mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, rej) => {
          reject = rej;
        }),
    );
    const first = useSettings.getState().update("terminal_font_size", 18);
    // 写入已经发出去、还没返回时，另一处把它改成了 22。
    await vi.waitFor(() => expect(mocks.settingsSet).toHaveBeenCalledTimes(1));
    useSettings.setState({ terminal_font_size: 22 });
    reject(new Error("写失败"));
    await first;
    expect(useSettings.getState().terminal_font_size).toBe(22);
  });
});

describe("同步、恢复之后刷新设置缓存", () => {
  it("同步完成（上次结果变了）：重新读设置和主机列表", async () => {
    useSync.setState({ status: status(100) });
    mocks.settingsGet.mockImplementation(async (key: string) =>
      key === "terminal_font_size" ? JSON.stringify(19) : null,
    );
    useSync.getState().apply(status(200));
    await vi.waitFor(() =>
      expect(useSettings.getState().terminal_font_size).toBe(19),
    );
    expect(mocks.vaultLoad).toHaveBeenCalled();
  });

  it("上次结果没变就不重读", async () => {
    useSync.setState({ status: status(100) });
    useSync.getState().apply(status(100));
    await Promise.resolve();
    expect(mocks.settingsGet).not.toHaveBeenCalled();
  });

  it("恢复备份、用另一份覆盖冲突之后：refreshLocalData 重读设置和主机", async () => {
    mocks.settingsGet.mockImplementation(async (key: string) =>
      key === "terminal_font_size" ? JSON.stringify(17) : null,
    );
    await refreshLocalData();
    expect(useSettings.getState().terminal_font_size).toBe(17);
    expect(mocks.vaultLoad).toHaveBeenCalled();
  });
});

// ---- R03：连续改两次、写入失败时，界面值必须等于数据库里的值 ----

describe("连续改执行模式（写入可能失败、可能乱序）", () => {
  type Outcome = "ok" | "fail";

  /** 假数据库：只有写成功才改；每次写入由测试决定成败和时机。 */
  function fakeDatabase(initial: string) {
    const db = { mode: initial };
    const writes: { value: string; settle: (outcome: Outcome) => void }[] = [];
    mocks.settingsGet.mockImplementation(async (key: string) =>
      key === "ai_exec_mode" ? JSON.stringify(db.mode) : null,
    );
    mocks.settingsSet.mockImplementation(
      (key: string, raw: string) =>
        new Promise<void>((resolve, reject) => {
          if (key !== "ai_exec_mode") {
            resolve();
            return;
          }
          writes.push({
            value: JSON.parse(raw) as string,
            settle: (outcome) => {
              if (outcome === "ok") {
                db.mode = JSON.parse(raw) as string;
                resolve();
              } else {
                reject(new Error("数据库写不进去"));
              }
            },
          });
        }),
    );
    return { db, writes };
  }

  /** 按调用顺序给第 i 次写入一个结果（等它真的发出来再给：写入按顺序进行时，第二次要等第一次结束）。 */
  async function settleInOrder(
    writes: { settle: (o: Outcome) => void }[],
    outcomes: Outcome[],
  ) {
    for (let index = 0; index < outcomes.length; index += 1) {
      await vi.waitFor(() => expect(writes.length).toBeGreaterThan(index));
      writes[index].settle(outcomes[index]);
      await Promise.resolve();
    }
  }

  const combos: [Outcome, Outcome][] = [
    ["ok", "ok"],
    ["ok", "fail"],
    ["fail", "ok"],
    ["fail", "fail"],
  ];

  it.each(combos)(
    "数据库是自动，改成手动再改回自动，两次写入结果 %s / %s：界面等于数据库",
    async (first, second) => {
      const { db, writes } = fakeDatabase("auto");
      await useSettings.getState().load();
      expect(useSettings.getState().ai_exec_mode).toBe("auto");

      const one = useSettings.getState().update("ai_exec_mode", "manual");
      const two = useSettings.getState().update("ai_exec_mode", "auto");
      await settleInOrder(writes, [first, second]);
      await Promise.all([one, two]);

      expect(useSettings.getState().ai_exec_mode).toBe(db.mode);
    },
  );

  it("三次连续修改全部失败，界面回到数据库里的值", async () => {
    const { db, writes } = fakeDatabase("auto");
    await useSettings.getState().load();
    const calls = [
      useSettings.getState().update("ai_exec_mode", "manual"),
      useSettings.getState().update("ai_exec_mode", "auto"),
      useSettings.getState().update("ai_exec_mode", "manual"),
    ];
    await settleInOrder(writes, ["fail", "fail", "fail"]);
    await Promise.all(calls);
    expect(db.mode).toBe("auto");
    expect(useSettings.getState().ai_exec_mode).toBe("auto");
  });

  it("同一项设置的写入按顺序进行：前一次没结束，后一次不会发出去", async () => {
    const { db, writes } = fakeDatabase("manual");
    await useSettings.getState().load();
    const one = useSettings.getState().update("ai_exec_mode", "auto");
    const two = useSettings.getState().update("ai_exec_mode", "manual");
    await vi.waitFor(() => expect(writes.length).toBe(1));
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(writes.length, "第一次还没结束，第二次就发出去了").toBe(1);
    writes[0].settle("ok");
    await vi.waitFor(() => expect(writes.length).toBe(2));
    writes[1].settle("ok");
    await Promise.all([one, two]);
    expect(db.mode).toBe("manual");
    expect(useSettings.getState().ai_exec_mode).toBe("manual");
  });

  it("不同的设置互不排队", async () => {
    const fake = fakeDatabase("manual");
    await useSettings.getState().load();
    const mode = useSettings.getState().update("ai_exec_mode", "auto"); // 卡着不返回
    const size = await useSettings.getState().update("terminal_font_size", 18);
    expect(size).toBe(true);
    expect(useSettings.getState().terminal_font_size).toBe(18);
    // 收尾：放行卡住的那次写入，不影响后面的用例。
    await vi.waitFor(() => expect(fake.writes.length).toBe(1));
    fake.writes[0].settle("ok");
    await mode;
  });
});
