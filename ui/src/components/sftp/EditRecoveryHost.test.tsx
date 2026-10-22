/** @vitest-environment jsdom */
//! N04：点了"稍后处理"以后，这次运行里还能再打开恢复对话框；重新上传可以用这台主机
//! 任何一个已连接的 SFTP 会话（SFTP 标签、终端的文件侧栏都算）。
//!
//! 完整流程：启动时没有连接 → 点"稍后处理" → 连上主机 → 从入口重新进入 → 重新上传成功。

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { EditLeftover, EditLeftoverResult } from "../../api/sftp";
import type { SftpSessionInfo } from "../../state/editRecovery";
import { EditRecoveryHost } from "./EditRecoveryHost";

afterEach(cleanup);

const item: EditLeftover = {
  id: "e1",
  file_name: "app.conf",
  size: 10,
  remote_path: "/etc/app.conf",
  host_id: 3,
  host_label: "测试主机",
};

function setup(
  sessions: SftpSessionInfo[],
  leftovers: EditLeftover[] = [item],
) {
  const load = vi.fn(async () => leftovers);
  const act = vi.fn(async (): Promise<EditLeftoverResult> => ({
    state: "done",
    message: "10 字节",
  }));
  const view = render(
    <EditRecoveryHost sessions={sessions} load={load} act={act} />,
  );
  const rerender = (next: SftpSessionInfo[]) =>
    view.rerender(<EditRecoveryHost sessions={next} load={load} act={act} />);
  return { load, act, rerender };
}

describe("恢复入口", () => {
  it("启动时没连接 → 稍后处理 → 连上 → 从入口回来 → 重新上传成功", async () => {
    const { act, rerender } = setup([]);

    // 启动：对话框自动出现，但那台主机还没连接，不能重新上传。
    await screen.findByText("上次有 1 个文件的修改没有上传");
    expect((screen.getByText("重新上传") as HTMLButtonElement).disabled).toBe(
      true,
    );

    // 点"稍后处理"：对话框收起，但留下一个常驻入口。
    fireEvent.click(screen.getByText("稍后处理"));
    await waitFor(() =>
      expect(screen.queryByText("上次有 1 个文件的修改没有上传")).toBeNull(),
    );
    const entry = await screen.findByTestId("edit-recovery-entry");
    expect(entry.textContent).toContain("1 个未上传的修改");

    // 用户连上了那台主机（出现一个已连接的 SFTP 会话）。
    rerender([{ sftpId: "s-1", hostId: 3, ready: true }]);

    // 从入口回来：对话框再打开，现在能重新上传。
    fireEvent.click(screen.getByTestId("edit-recovery-entry"));
    await screen.findByText("上次有 1 个文件的修改没有上传");
    const button = screen.getByText("重新上传") as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    fireEvent.click(button);
    await waitFor(() =>
      expect(act).toHaveBeenCalledWith(item, "reupload", "s-1"),
    );

    // 传完：这一项消失，入口也没了。
    await waitFor(() =>
      expect(screen.queryByTestId("edit-recovery-entry")).toBeNull(),
    );
    expect(screen.queryByText("上次有 1 个文件的修改没有上传")).toBeNull();
  });

  it("终端的文件侧栏（停靠会话）连着这台主机时，同样可以重新上传", async () => {
    const { act } = setup([
      { sftpId: "dock-9", hostId: 3, ready: true, via: "dock" },
      { sftpId: "tab-other", hostId: 4, ready: true },
    ]);
    await screen.findByText("上次有 1 个文件的修改没有上传");
    fireEvent.click(screen.getByText("重新上传"));
    await waitFor(() =>
      expect(act).toHaveBeenCalledWith(item, "reupload", "dock-9"),
    );
  });

  it("还没连好的会话不算（连接中不能传）", async () => {
    setup([{ sftpId: "s-1", hostId: 3, ready: false }]);
    await screen.findByText("上次有 1 个文件的修改没有上传");
    expect((screen.getByText("重新上传") as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it("没有未上传的修改时什么都不显示", async () => {
    const { load } = setup([], []);
    await waitFor(() => expect(load).toHaveBeenCalled());
    expect(screen.queryByTestId("edit-recovery-entry")).toBeNull();
    expect(screen.queryByText(/个文件的修改没有上传/)).toBeNull();
  });
});
