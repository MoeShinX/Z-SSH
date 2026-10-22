import { describe, expect, it } from "vitest";

import {
  activeAfterClose,
  fileTabLabel,
  handoffOf,
  isOutsideWindow,
  sessionsOf,
  tabFromHandoff,
  type FileWindowTab,
} from "./fileWindowTabs";

const handoff = (sftp_id: string, host_id = 1) => ({
  sftp_id,
  host_id,
  title: "web-1（文件）",
  local: "C:\\Users\\me",
  remote: "/root",
});

describe("文件窗口的标签", () => {
  it("交过来的标签：有会话就直接能用，没有就要自己去连", () => {
    expect(tabFromHandoff(handoff("s1"), "t1")).toMatchObject({
      id: "t1",
      sftpId: "s1",
      phase: "ready",
      paths: { local: "C:\\Users\\me", remote: "/root" },
    });
    expect(tabFromHandoff(handoff(""), "t2").phase).toBe("connecting");
  });

  it("只报已经连上的会话", () => {
    const tabs: FileWindowTab[] = [
      tabFromHandoff(handoff("s1", 1), "a"),
      tabFromHandoff(handoff("", 2), "b"),
      tabFromHandoff(handoff("s3", 3), "c"),
    ];
    expect(sessionsOf(tabs)).toEqual([
      { sftp_id: "s1", host_id: 1 },
      { sftp_id: "s3", host_id: 3 },
    ]);
  });

  it("关掉当前标签选左边的，没有就选右边的", () => {
    const tabs = ["a", "b", "c"].map((id) => tabFromHandoff(handoff(id), id));
    expect(activeAfterClose(tabs, "b", "b")).toBe("a");
    expect(activeAfterClose(tabs, "a", "a")).toBe("b");
    expect(activeAfterClose(tabs, "c", "a")).toBe("a");
    expect(activeAfterClose([tabs[0]], "a", "a")).toBe("");
  });

  it("移回主窗口时带上现在的目录；没连上的不带会话", () => {
    const tab = tabFromHandoff(handoff("s1"), "a");
    expect(handoffOf(tab, { local: "D:\\x", remote: "/etc" })).toEqual({
      sftp_id: "s1",
      host_id: 1,
      title: "web-1（文件）",
      local: "D:\\x",
      remote: "/etc",
    });
    expect(handoffOf({ ...tab, phase: "failed" }, undefined).sftp_id).toBe("");
  });

  it("松开的位置在窗口外面才算拖出去", () => {
    expect(isOutsideWindow(10, 10, 800, 600)).toBe(false);
    expect(isOutsideWindow(-1, 10, 800, 600)).toBe(true);
    expect(isOutsideWindow(10, 600, 800, 600)).toBe(true);
  });

  it("文件窗口的标签只显示主机名", () => {
    expect(fileTabLabel("web-1 (SFTP)")).toBe("web-1");
    expect(fileTabLabel("web-1（文件）")).toBe("web-1");
    expect(fileTabLabel("web-1")).toBe("web-1");
  });
});
