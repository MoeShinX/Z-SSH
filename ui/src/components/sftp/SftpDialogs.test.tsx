/** @vitest-environment jsdom */
//! SFTP 对话框逻辑的单测（M3-2 第 6.1 节）：
//! 冲突对话框的选项逻辑（续传什么时候可用、"对后续都这样"）、权限对话框的
//! 勾选和八进制联动、队列各状态的显示。
//! M3-4："下载到…"的选择文件夹按钮（系统对话框）。

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(cleanup);

const dialogMocks = vi.hoisted(() => ({ pickFolder: vi.fn() }));
vi.mock("../../api/dialog", () => ({ pickFolder: dialogMocks.pickFolder }));

import {
  editConflictMessage,
  initialPermissionBits,
  conflictDefaultAction,
  conflictResumeEnabled,
  isCopyConflict,
  matrixToOctal,
  octalToMatrix,
  parseOctalInput,
  SftpDownloadToDialog,
  validateLocalDir,
  SftpDeleteConfirm,
} from "./SftpDialogs";

describe("冲突对话框的选项逻辑", () => {
  it("续传只有目标比源小时可用（can_resume 由后端算好）", () => {
    expect(conflictResumeEnabled({ can_resume: true })).toBe(true);
    expect(conflictResumeEnabled({ can_resume: false })).toBe(false);
  });

  it("默认动作：能续传建议续传，不然覆盖", () => {
    expect(conflictDefaultAction({ can_resume: true })).toBe("resume");
    expect(conflictDefaultAction({ can_resume: false })).toBe("overwrite");
  });

  it("复制粘贴的冲突：默认「重命名」，没有续传，文案说复制不说传", async () => {
    expect(conflictDefaultAction({ can_resume: false, direction: "remote_copy" })).toBe("rename");
    expect(conflictDefaultAction({ can_resume: false, direction: "local_copy" })).toBe("rename");
    expect(conflictDefaultAction({ can_resume: false, direction: "upload" })).toBe("overwrite");
    expect(isCopyConflict({ direction: "remote_copy" })).toBe(true);
    expect(isCopyConflict({ direction: "download" })).toBe(false);

    const { SftpConflictDialog } = await import("./SftpDialogs");
    const { getByText, queryByText, getByDisplayValue } = render(
      <SftpConflictDialog
        prompt={{
          conflict_id: 1,
          job: 0,
          direction: "remote_copy",
          source: "/d/a.txt",
          target: "/d/a.txt",
          can_resume: false,
          suggested_name: "a - 副本.txt",
        }}
        onAnswer={() => {}}
        onDismissAsSkip={() => {}}
      />,
    );
    expect(getByText("源（要复制的）")).toBeTruthy();
    expect(getByText("换个名字再复制")).toBeTruthy();
    expect(queryByText("续传")).toBeNull();
    expect(getByDisplayValue("a - 副本.txt")).toBeTruthy();
  });

  it("四个动作在回答里都合法（skip 的 apply_to_all 可以是 true 或 false）", () => {
    // "对后续所有冲突都这样处理"对每个动作都成立（引擎按动作记）。
    const actions = ["overwrite", "skip", "rename", "resume"] as const;
    for (const action of actions) {
      const answer = { action, applyToAll: true };
      expect(answer.applyToAll).toBe(true);
    }
  });
});

describe("权限对话框：勾选和八进制联动", () => {
  it("勾选矩阵 → 八进制", () => {
    // rwxr-x---
    expect(
      matrixToOctal({
        owner: { read: true, write: true, execute: true },
        group: { read: true, write: false, execute: true },
        others: { read: false, write: false, execute: false },
      }),
    ).toBe(0o750);
    // ---------
    expect(
      matrixToOctal({
        owner: { read: false, write: false, execute: false },
        group: { read: false, write: false, execute: false },
        others: { read: false, write: false, execute: false },
      }),
    ).toBe(0);
    // rw-r--r--
    expect(matrixToOctal(octalToMatrix(0o644)!)).toBe(0o644);
  });

  it("八进制 → 勾选矩阵（来回转换不变形）", () => {
    for (const mode of [0o644, 0o755, 0o600, 0o777, 0o000, 0o750]) {
      expect(matrixToOctal(octalToMatrix(mode)!)).toBe(mode);
    }
  });

  it("非法的八进制输入返回 null（3 位或 4 位 0-7 才行）", () => {
    expect(parseOctalInput("644")).toBe(0o644);
    expect(parseOctalInput("0640")).toBe(0o640);
    expect(parseOctalInput("0777")).toBe(0o777);
    expect(parseOctalInput("899")).toBeNull();
    expect(parseOctalInput("abc")).toBeNull();
    expect(parseOctalInput("99")).toBeNull();
    expect(parseOctalInput("12345")).toBeNull();
    expect(parseOctalInput("")).toBeNull();
    // 0o777 以外的值不进矩阵（4 位的第一位是 setuid 之类的位，这里不支持）
    expect(octalToMatrix(0o1000)).toBeNull();
    expect(octalToMatrix(-1)).toBeNull();
  });

  it("每个位的含义：读 4 / 写 2 / 执行 1", () => {
    const matrix = octalToMatrix(0o640)!;
    expect(matrix.owner).toEqual({ read: true, write: true, execute: false });
    expect(matrix.group).toEqual({ read: true, write: false, execute: false });
    expect(matrix.others).toEqual({ read: false, write: false, execute: false });
  });
});

describe("权限对话框的初始值", () => {
  it("服务器给的 mode 带文件类型位：只取低 9 位", () => {
    expect(initialPermissionBits(0o100644)).toBe(0o644);
    expect(initialPermissionBits(0o040755)).toBe(0o755);
    expect(initialPermissionBits(0o100600)).toBe(0o600);
  });

  it("没有 mode 就用 644", () => {
    expect(initialPermissionBits(undefined)).toBe(0o644);
  });

  it("带类型位的 mode 转出来的勾选矩阵和不带的一致（以前会被当成非法值重置）", () => {
    const withType = octalToMatrix(initialPermissionBits(0o100640));
    expect(withType).toEqual(octalToMatrix(0o640));
  });
});

describe("下载到…的目录检查", () => {
  it("空的、带 Windows 不允许的字符的不行；盘符冒号、中文、空格可以", () => {
    expect(validateLocalDir("").ok).toBe(false);
    expect(validateLocalDir("   ").ok).toBe(false);
    expect(validateLocalDir("D:\\a|b").ok).toBe(false);
    expect(validateLocalDir("D:\\下载 目录\\x").ok).toBe(true);
    expect(validateLocalDir("C:\\").ok).toBe(true);
  });
});

describe("编辑器冲突提示正文", () => {
  it("大小和时间写进句子，全角右括号后面不再紧跟句号", () => {
    const text = editConflictMessage({
      remote_path: "/tmp/x/edit-me.txt",
      remote_gone: false,
      remote_size: 24,
      remote_mtime: 1760709190,
    });
    expect(text).toContain("「edit-me.txt」");
    expect(text).toContain("现在 24 B");
    expect(text).not.toMatch(/）\s*。/);
    expect(text).not.toMatch(/\s。/);
    expect(text).toContain("这次保存还没有上传");
  });

  it("远端被删的说法", () => {
    const text = editConflictMessage({
      remote_path: "/tmp/gone.txt",
      remote_gone: true,
      remote_size: undefined,
      remote_mtime: undefined,
    });
    expect(text).toContain("被删除了");
    expect(text).not.toMatch(/\s。/);
  });
});

describe("下载到…的选择文件夹按钮", () => {
  const renderDialog = () => {
    const onConfirm = vi.fn(async () => null);
    const view = render(
      <SftpDownloadToDialog count={2} initialDir="D:/work" onConfirm={onConfirm} onCancel={() => {}} />,
    );
    return { ...view, onConfirm };
  };

  it("点按钮弹系统对话框，选中的目录填进输入框；输入框仍可手输", async () => {
    dialogMocks.pickFolder.mockResolvedValueOnce("E:/picked");
    renderDialog();
    const input = document.body.querySelector("input") as HTMLInputElement;
    expect(input.value).toBe("D:/work");
    fireEvent.click(document.body.querySelector('[data-action="pick-folder"]') as HTMLElement);
    await waitFor(() => expect(input.value).toBe("E:/picked"));
    expect(dialogMocks.pickFolder).toHaveBeenCalledWith("D:/work", "选择下载到的文件夹");
    fireEvent.change(input, { target: { value: "F:/typed" } });
    expect(input.value).toBe("F:/typed");
  });

  it("用户取消对话框：输入框保持原样", async () => {
    dialogMocks.pickFolder.mockResolvedValueOnce(null);
    renderDialog();
    const input = document.body.querySelector("input") as HTMLInputElement;
    fireEvent.click(document.body.querySelector('[data-action="pick-folder"]') as HTMLElement);
    await waitFor(() => expect(dialogMocks.pickFolder).toHaveBeenCalled());
    expect(input.value).toBe("D:/work");
  });
});

describe("删除确认", () => {
  it("列出要删的名字，太多时只列前 8 个并写明还有几项", () => {
    const names = Array.from({ length: 11 }, (_, i) => `file-${i}.log`);
    const { getByText, queryByText } = render(
      <SftpDeleteConfirm
        count={names.length}
        names={names}
        hasFolder={false}
        isRemote
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(getByText("file-0.log")).toBeTruthy();
    expect(getByText("file-7.log")).toBeTruthy();
    expect(queryByText("file-8.log")).toBeNull();
    expect(getByText("…等另外 3 项")).toBeTruthy();
  });
});
