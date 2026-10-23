import { describe, expect, it } from "vitest";

import {
  countConnections,
  decideClose,
  DEFAULT_CLOSE_ACTION,
  describeConnections,
  NO_CONNECTIONS,
  parseCloseAction,
  parseFileWindowCloseAction,
  quitConfirmText,
  quitNotes,
  quitVerdict,
  shouldMergeOnClose,
  type QuitState,
} from "./closeBehavior";
import type { PaneSession, Tab } from "./sessions";

describe("parseCloseAction", () => {
  it("缺省或读不懂的值按默认：缩到托盘", () => {
    expect(DEFAULT_CLOSE_ACTION).toBe("tray");
    for (const raw of [undefined, null, "", "Quit", 1, true, "tray", "close", {}]) {
      expect(parseCloseAction(raw)).toBe("tray");
    }
  });

  it("明确选了退出才退出", () => {
    expect(parseCloseAction("quit")).toBe("quit");
  });
});

describe("decideClose", () => {
  it("缩到托盘模式：隐藏，不走任何确认", () => {
    expect(decideClose({ action: "tray", quitting: false, hintShown: true })).toEqual({
      type: "hide",
      showHint: false,
    });
  });

  it("第一次缩到托盘才要提示，之后不再提示", () => {
    expect(decideClose({ action: "tray", quitting: false, hintShown: false })).toEqual({
      type: "hide",
      showHint: true,
    });
    // 提示过之后（调用方把 hintShown 记成 true）
    expect(decideClose({ action: "tray", quitting: false, hintShown: true })).toEqual({
      type: "hide",
      showHint: false,
    });
  });

  it("退出模式：走原来的退出流程，不提示", () => {
    expect(decideClose({ action: "quit", quitting: false, hintShown: false })).toEqual({
      type: "quit",
    });
  });

  it("托盘退出 / 已确认退出时，即使设置是缩到托盘也不能再隐藏", () => {
    expect(decideClose({ action: "tray", quitting: true, hintShown: false })).toEqual({
      type: "quit",
    });
  });
});

describe("quitVerdict（和 UI-4 之前 onCloseRequested 的判断一致）", () => {
  const idle: QuitState = {
    unsavedDocs: 0,
    transfers: 0,
    dirtyEdits: 0,
    connections: NO_CONNECTIONS,
    confirmed: false,
  };

  it("什么都没有：放行", () => {
    expect(quitVerdict(idle)).toEqual({ type: "go" });
  });

  it("先问未保存的文档", () => {
    expect(quitVerdict({ ...idle, unsavedDocs: 2, transfers: 1 })).toEqual({
      type: "ask-unsaved",
    });
  });

  it("没有未保存文档，但有传输或远程编辑未回传：问放弃", () => {
    expect(quitVerdict({ ...idle, transfers: 1 })).toEqual({ type: "ask-busy" });
    expect(quitVerdict({ ...idle, dirtyEdits: 3 })).toEqual({ type: "ask-busy" });
  });

  it("已经确认过：放行", () => {
    expect(
      quitVerdict({
        unsavedDocs: 1,
        transfers: 1,
        dirtyEdits: 1,
        connections: { terminals: 2, forwards: 1, sftp: 1 },
        confirmed: true,
      }),
    ).toEqual({ type: "go" });
  });

  it("只有活动连接（终端、端口转发、SFTP 任一类）：也要确认", () => {
    for (const connections of [
      { terminals: 1, forwards: 0, sftp: 0 },
      { terminals: 0, forwards: 1, sftp: 0 },
      { terminals: 0, forwards: 0, sftp: 1 },
    ]) {
      expect(quitVerdict({ ...idle, connections })).toEqual({ type: "ask-busy" });
    }
  });

  it("未保存文档和连接同时有：还是只有一个对话框（未保存那个），连接写在 quitNotes 里", () => {
    const state = { ...idle, unsavedDocs: 1, connections: { terminals: 3, forwards: 1, sftp: 0 } };
    expect(quitVerdict(state)).toEqual({ type: "ask-unsaved" });
    expect(quitNotes(state)).toEqual(["还有 4 个连接（3 个终端、1 个端口转发），退出会全部断开。"]);
  });
});

describe("退出确认的文字", () => {
  const idle: QuitState = {
    unsavedDocs: 0,
    transfers: 0,
    dirtyEdits: 0,
    connections: NO_CONNECTIONS,
    confirmed: false,
  };

  it("连接按类别写，只列有的", () => {
    expect(describeConnections({ terminals: 3, forwards: 1, sftp: 0 })).toBe("3 个终端、1 个端口转发");
    expect(describeConnections({ terminals: 0, forwards: 0, sftp: 2 })).toBe("2 个 SFTP 连接");
    expect(describeConnections(NO_CONNECTIONS)).toBe("");
  });

  it("只有传输 / 远程编辑时沿用原来的标题和按钮", () => {
    expect(quitConfirmText({ ...idle, transfers: 2 })).toEqual({
      title: "还有 2 个传输未完成",
      message: "退出程序会取消这些传输。",
      confirmLabel: "退出并取消",
    });
    expect(quitConfirmText({ ...idle, dirtyEdits: 1 }).confirmLabel).toBe("退出并放弃");
  });

  it("有连接：问「确定退出」，按钮是「退出」；传输、远程编辑的后果列在同一个对话框里", () => {
    const text = quitConfirmText({
      ...idle,
      transfers: 1,
      dirtyEdits: 2,
      connections: { terminals: 3, forwards: 1, sftp: 0 },
    });
    expect(text.confirmLabel).toBe("退出");
    expect(text.message.split("\n")).toEqual([
      "还有 4 个连接（3 个终端、1 个端口转发），退出会全部断开。",
      "还有 1 个传输未完成，退出会取消这些传输。",
      "还有 2 个文件的修改还没上传，退出会丢掉这些修改。",
      "确定退出？",
    ]);
  });
});

describe("countConnections", () => {
  const pane = (phase: PaneSession["phase"]) => ({ phase });
  const terminalTab = (...ids: string[]): Pick<Tab, "tree" | "sftp" | "editor"> => ({
    tree:
      ids.length === 1
        ? { kind: "pane", id: ids[0] }
        : {
            kind: "split",
            id: "split-1",
            axis: "x",
            ratio: 0.5,
            first: { kind: "pane", id: ids[0] },
            second: { kind: "pane", id: ids[1] },
          },
  });

  it("终端：已连接和正在连接的算，空 pane、已断开的不算；分屏每个 pane 各算一个", () => {
    const counts = countConnections({
      tabs: [terminalTab("a", "b"), terminalTab("c"), terminalTab("d")],
      panes: {
        a: pane("connected"),
        b: pane("authenticating"),
        c: pane("disconnected"),
        d: pane("idle"),
      },
      fileWindowSessions: 0,
      dockPhases: [],
      forwardPhases: [],
    });
    expect(counts).toEqual({ terminals: 2, forwards: 0, sftp: 0 });
  });

  it("SFTP 标签和 SFTP 编辑器标签里占位的 pane 不算终端；SFTP 数标签加文件窗口的会话", () => {
    const sftpTab = (phase: "picking" | "connecting" | "ready" | "failed") => ({
      ...terminalTab("s" + phase),
      sftp: { sftpId: "x", hostId: 1, phase, status: "" },
    });
    const counts = countConnections({
      tabs: [
        sftpTab("ready"),
        sftpTab("connecting"),
        sftpTab("failed"),
        sftpTab("picking"),
        { ...terminalTab("e"), editor: { editId: 1 } },
      ],
      panes: {
        sready: pane("connected"),
        sconnecting: pane("connected"),
        e: pane("connected"),
      },
      fileWindowSessions: 2,
      dockPhases: [],
      forwardPhases: [],
    });
    expect(counts).toEqual({ terminals: 0, forwards: 0, sftp: 4 });
  });

  it("终端旁停靠的文件面板：正在连、已就绪的算 SFTP，失败的不算；终端断了也照算", () => {
    const counts = countConnections({
      tabs: [terminalTab("a")],
      panes: { a: pane("disconnected") },
      fileWindowSessions: 1,
      dockPhases: ["ready", "connecting", "failed"],
      forwardPhases: [],
    });
    expect(counts).toEqual({ terminals: 0, forwards: 0, sftp: 3 });
  });

  it("端口转发：启动中、运行中算，失败和其他不算", () => {
    const counts = countConnections({
      tabs: [],
      panes: {},
      fileWindowSessions: 0,
      dockPhases: [],
      forwardPhases: ["starting", "running", "failed", "stopped"],
    });
    expect(counts.forwards).toBe(2);
  });
});

describe("关闭 Z-SFTP 文件窗口", () => {
  it("缺省和读不懂的值都按「断开并关闭」，只有 merge 才合并", () => {
    expect(parseFileWindowCloseAction(undefined)).toBe("close");
    expect(parseFileWindowCloseAction("")).toBe("close");
    expect(parseFileWindowCloseAction("something-else")).toBe("close");
    expect(parseFileWindowCloseAction("close")).toBe("close");
    expect(parseFileWindowCloseAction("merge")).toBe("merge");
  });

  it("合并只在：选了合并、窗口里有标签、不是正在真的关", () => {
    expect(shouldMergeOnClose("merge", 2, false)).toBe(true);
    expect(shouldMergeOnClose("close", 2, false)).toBe(false);
    expect(shouldMergeOnClose("merge", 0, false)).toBe(false);
    expect(shouldMergeOnClose("merge", 2, true)).toBe(false);
  });
});
