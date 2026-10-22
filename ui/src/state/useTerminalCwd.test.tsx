/** @vitest-environment jsdom */
//! 目录跟随的轮询（PERF-2 第 1 项）：目录没变不更新状态；切路径、切会话、失败再恢复仍然正确。

import { useEffect } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FOLLOW_POLL_MS, mergeTerminalCwd, useTerminalCwd, type TerminalCwd } from "./followCwd";

describe("mergeTerminalCwd", () => {
  it("会话和目录都一样：返回原对象", () => {
    const prev = { sessionId: "s1", cwd: "/srv" };
    expect(mergeTerminalCwd(prev, "s1", "/srv")).toBe(prev);
  });

  it("重复的 null（查询失败）也返回原对象", () => {
    const prev = { sessionId: "s1", cwd: null };
    expect(mergeTerminalCwd(prev, "s1", null)).toBe(prev);
  });

  it("目录变了、会话变了、失败后恢复：都是新对象", () => {
    const prev = { sessionId: "s1", cwd: "/srv" };
    expect(mergeTerminalCwd(prev, "s1", "/tmp")).toEqual({ sessionId: "s1", cwd: "/tmp" });
    expect(mergeTerminalCwd(prev, "s2", "/srv")).toEqual({ sessionId: "s2", cwd: "/srv" });
    expect(mergeTerminalCwd(prev, "s1", null)).toEqual({ sessionId: "s1", cwd: null });
    expect(mergeTerminalCwd({ sessionId: "s1", cwd: null }, "s1", "/srv")).toEqual({
      sessionId: "s1",
      cwd: "/srv",
    });
    expect(mergeTerminalCwd(null, "s1", null)).toEqual({ sessionId: "s1", cwd: null });
  });
});

describe("useTerminalCwd", () => {
  let commits = 0;
  let latest: TerminalCwd | null = null;

  /** 模拟 App 下面的子树（比如终端窗格）：父层的状态没换，React 就不会再渲染它。 */
  function Child() {
    commits += 1;
    return null;
  }
  function Host(props: { sessionId: string | null; query: (id: string) => Promise<string | null> }) {
    const value = useTerminalCwd(props.sessionId, props.query);
    useEffect(() => {
      latest = value;
    });
    return <Child />;
  }
  const ui = (sessionId: string | null, query: (id: string) => Promise<string | null>) => (
    <Host sessionId={sessionId} query={query} />
  );
  /** 让一个轮询周期走完（定时器 + 查询的 promise）。 */
  const cycle = () =>
    act(async () => {
      await vi.advanceTimersByTimeAsync(FOLLOW_POLL_MS);
    });

  beforeEach(() => {
    vi.useFakeTimers();
    commits = 0;
    latest = null;
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("同一路径停留多个周期：第一次结果之后子树不再渲染", async () => {
    const query = vi.fn(async () => "/srv/app");
    render(ui("s1", query));
    await act(async () => {});
    expect(latest).toEqual({ sessionId: "s1", cwd: "/srv/app" });
    const stable = latest;
    const before = commits;
    for (let i = 0; i < 4; i += 1) await cycle();
    expect(query.mock.calls.length).toBeGreaterThanOrEqual(5);
    expect(commits).toBe(before);
    expect(latest).toBe(stable);
  });

  it("重复失败子树不再渲染；路径变化、失败后恢复仍然更新", async () => {
    const results: Array<string | Error> = ["/a", new Error("x"), new Error("y"), new Error("z"), "/a", "/b"];
    const query = vi.fn(async () => {
      const next = results.shift() ?? "/b";
      if (next instanceof Error) throw next;
      return next;
    });
    render(ui("s1", query));
    await act(async () => {});
    expect(latest).toEqual({ sessionId: "s1", cwd: "/a" });

    await cycle();
    expect(latest).toEqual({ sessionId: "s1", cwd: null });
    const afterFirstFailure = commits;
    await cycle();
    await cycle();
    expect(commits).toBe(afterFirstFailure);

    await cycle();
    expect(latest).toEqual({ sessionId: "s1", cwd: "/a" });
    await cycle();
    expect(latest).toEqual({ sessionId: "s1", cwd: "/b" });
  });

  it("切换会话：立即用新会话查询，结果带新会话号；旧会话迟到的结果不写入", async () => {
    let resolveOld: (value: string | null) => void = () => {};
    const query = vi.fn((id: string) =>
      id === "s1"
        ? new Promise<string | null>((resolve) => {
            resolveOld = resolve;
          })
        : Promise.resolve("/srv"),
    );
    const view = render(ui("s1", query));
    await act(async () => {});
    view.rerender(ui("s2", query));
    await act(async () => {});
    expect(latest).toEqual({ sessionId: "s2", cwd: "/srv" });
    await act(async () => {
      resolveOld("/old");
    });
    expect(latest).toEqual({ sessionId: "s2", cwd: "/srv" });
  });

  it("sessionId 为 null：不轮询", async () => {
    const query = vi.fn(async () => "/srv");
    render(ui(null, query));
    await cycle();
    expect(query).not.toHaveBeenCalled();
  });
});
