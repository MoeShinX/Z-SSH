//! 停靠判定和目录搬运（M3-3 第 6.1 节）：同一台主机才能停靠；目录原样带走。

import { describe, expect, it } from "vitest";

import { canDockOnto, carryPaths, hostsInTab, newDockSessionDefaults } from "./dock";
import { emptyPane, type PaneSession, type Tab } from "./sessions";
import { splitPane } from "./splitTree";

function hostPane(hostId: number): PaneSession {
  return emptyPane({ kind: "host", hostId, label: `h${hostId}` });
}

function terminalTab(id: string, paneIdsAndHosts: Array<[string, number | null]>): {
  tab: Tab;
  panes: Record<string, PaneSession>;
} {
  const panes: Record<string, PaneSession> = {};
  for (const [paneId, host] of paneIdsAndHosts) {
    panes[paneId] = host === null ? emptyPane() : hostPane(host);
  }
  const [first, ...rest] = paneIdsAndHosts;
  let tree: Tab["tree"] = { kind: "pane", id: first[0] };
  for (const [paneId] of rest) {
    tree = splitPane(tree, first[0], paneId, `split-${paneId}`, "x");
  }
  return { tab: { id, title: id, tree, focus: first[0] }, panes };
}

describe("canDockOnto", () => {
  it("同一台主机的终端标签：可以停靠，并给出该主机的 pane", () => {
    const { tab, panes } = terminalTab("t1", [["p1", 7]]);
    expect(canDockOnto(7, tab, panes)).toEqual({ ok: true, paneId: "p1" });
  });

  it("别的主机：不能停靠（界面上不显示停靠提示，改提示原因）", () => {
    const { tab, panes } = terminalTab("t1", [["p1", 8]]);
    expect(canDockOnto(7, tab, panes)).toEqual({ ok: false, reason: "other-host" });
  });

  it("目标是另一个 SFTP 标签 / 没有主机记录的快速连接：不能停靠", () => {
    const sftpTab: Tab = {
      id: "s",
      title: "s",
      tree: { kind: "pane", id: "x" },
      focus: "x",
      sftp: { sftpId: "a", hostId: 7, phase: "ready", status: "" },
    };
    expect(canDockOnto(7, sftpTab, {})).toEqual({ ok: false, reason: "not-terminal" });
    const quick = terminalTab("t2", [["p1", null]]);
    expect(canDockOnto(7, quick.tab, quick.panes)).toEqual({ ok: false, reason: "other-host" });
  });

  it("分屏标签里焦点 pane 不是这台主机，但另一个 pane 是：停靠到那个 pane", () => {
    const { tab, panes } = terminalTab("t1", [
      ["p1", 8],
      ["p2", 7],
    ]);
    expect(canDockOnto(7, tab, panes)).toEqual({ ok: true, paneId: "p2" });
    expect(hostsInTab(tab, panes)).toEqual([8, 7]);
  });
});

describe("carryPaths", () => {
  it("两栏的目录原样带走；没记过的用空串", () => {
    expect(carryPaths({ local: "D:/work", remote: "/srv/app" })).toEqual({
      local: "D:/work",
      remote: "/srv/app",
    });
    expect(carryPaths(undefined)).toEqual({ local: "", remote: "" });
  });
});

describe("newDockSessionDefaults", () => {
  it("新开的停靠会话默认不跟随终端目录（要用户自己勾上），两栏目录用默认", () => {
    expect(newDockSessionDefaults()).toEqual({
      paths: { local: "", remote: "" },
      follow: false,
    });
  });

  it("每次给的是新对象，改一个不影响别的会话", () => {
    const first = newDockSessionDefaults();
    first.paths.remote = "/srv";
    expect(newDockSessionDefaults().paths.remote).toBe("");
  });
});
