import { describe, expect, it } from "vitest";

import type { EditLeftover } from "../api/sftp";
import {
  editToastFor,
  ownerPromptText,
  recoveryLine,
  recoveryTitle,
  reuploadState,
  collectSftpSessions,
  sftpSessionFor,
  uncertainNote,
} from "./editRecovery";

const known: EditLeftover = {
  id: "e1",
  file_name: "app.conf",
  size: 12,
  remote_path: "/etc/app.conf",
  host_id: 3,
  host_label: "测试主机",
};
const unknown: EditLeftover = { id: "e2", file_name: "mystery.txt", size: 1 };

describe("上次没上传的修改", () => {
  it("标题带数量，说明带主机和路径", () => {
    expect(recoveryTitle(2)).toBe("上次有 2 个文件的修改没有上传");
    expect(recoveryLine(known)).toBe("测试主机：/etc/app.conf");
    expect(recoveryLine(unknown)).toContain("mystery.txt");
  });

  it("这台主机上任何一个已经连好的 SFTP 会话都行（标签、文件侧栏）", () => {
    const sessions = collectSftpSessions(
      [
        { sftp: { sftpId: "a", hostId: 9, phase: "ready" } },
        { sftp: { sftpId: "b", hostId: 3, phase: "connecting" } },
        {},
      ],
      {
        3: { sftpId: "dock-3", phase: "ready" },
        5: { sftpId: "", phase: "connecting" },
      },
    );
    expect(sftpSessionFor(sessions, 3)).toBe("dock-3");
    expect(sftpSessionFor(sessions, 9)).toBe("a");
    expect(sftpSessionFor(sessions, 5)).toBeNull();
    expect(sftpSessionFor(sessions, 4)).toBeNull();
    expect(sftpSessionFor(sessions, undefined)).toBeNull();
    // 标签和侧栏都连好了，哪个都行。
    const both = collectSftpSessions(
      [{ sftp: { sftpId: "tab-3", hostId: 3, phase: "ready" } }],
      { 3: { sftpId: "dock-3", phase: "ready" } },
    );
    expect(["tab-3", "dock-3"]).toContain(sftpSessionFor(both, 3));
  });

  it("重新上传不可用时说明原因", () => {
    expect(reuploadState(known, "c")).toEqual({ enabled: true });
    expect(reuploadState(known, null).hint).toContain("测试主机");
    expect(reuploadState(unknown, "c").enabled).toBe(false);
  });

  it("没法确认有没有改过的留下来时，说明原因；确认过的不说", () => {
    expect(uncertainNote({ ...known, uncertain: true })).toContain("没法确认");
    expect(uncertainNote(known)).toBeNull();
  });

  it("恢复记录没写成时弹警告，说崩溃后可能无法恢复", () => {
    const toast = editToastFor({
      state: "warning",
      remote_path: "/etc/app.conf",
      message: "没能写下恢复记录：崩溃后可能无法恢复这次修改",
    });
    expect(toast?.type).toBe("warning");
    expect(toast?.text).toContain("崩溃后可能无法恢复");
    expect(toast?.text).toContain("app.conf");
  });

  it("其他事件的提示不变", () => {
    expect(
      editToastFor({ state: "uploaded", remote_path: "/a/b.txt" }),
    ).toEqual({
      type: "success",
      text: "已上传「b.txt」",
    });
    expect(
      editToastFor({
        state: "failed",
        remote_path: "/a/b.txt",
        message: "断线",
      })?.type,
    ).toBe("error");
    expect(
      editToastFor({ state: "uploading", remote_path: "/a/b.txt" }),
    ).toBeNull();
  });

  it("属主没保持住时，『已上传』要带上警告，不能只说成功", () => {
    const toast = editToastFor({
      state: "uploaded",
      remote_path: "/etc/app.conf",
      owner: {
        from: "uid 1001 / gid 1002",
        to: "uid 1000 / gid 1000",
        error: "权限不够",
      },
    });
    expect(toast?.type).toBe("warning");
    expect(toast?.text).toContain("uid 1001 / gid 1002");
    expect(toast?.text).toContain("uid 1000 / gid 1000");
    expect(toast?.text).toContain("属主");
  });

  it("询问里写清楚属主从谁变成谁", () => {
    const text = ownerPromptText({
      owner: {
        from: "uid 1001 / gid 1002",
        to: "uid 1000 / gid 1000",
        error: "权限不够",
      },
    });
    expect(text).toContain("从 uid 1001 / gid 1002 变成 uid 1000 / gid 1000");
  });
});

describe("内置编辑器的恢复项", () => {
  const draft = {
    id: "d",
    file_name: "a.conf",
    size: 3,
    remote_path: "/etc/a.conf",
    host_id: 1,
    host_label: "主机",
    from_draft: true,
  };
  const local = { id: "l", file_name: "a.txt", size: 1, remote_path: "C:/x/a.txt", local: true };

  it("草稿有说明，本机文件写回原文件且不需要 SFTP 会话", async () => {
    const { draftNote, reuploadLabel } = await import("./editRecovery");
    expect(draftNote(draft)).toContain("还没保存");
    expect(draftNote({ ...draft, from_draft: false })).toBeNull();
    expect(reuploadLabel(local)).toBe("写回原文件");
    expect(reuploadLabel(draft)).toBe("重新上传");
    expect(reuploadState(local, null)).toEqual({ enabled: true });
    expect(recoveryLine(local)).toContain("本机文件");
  });
});
