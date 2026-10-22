/** @vitest-environment jsdom */
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { EditLeftover } from "../../api/sftp";
import { EditRecoveryDialog } from "./EditRecoveryDialog";

afterEach(cleanup);

const item: EditLeftover = {
  id: "e1",
  file_name: "app.conf",
  size: 10,
  remote_path: "/etc/app.conf",
  host_id: 3,
  host_label: "测试主机",
};
const readySessions = [{ sftpId: "s1", hostId: 3, ready: true }];

describe("EditRecoveryDialog", () => {
  it("显示数量、主机和路径，三种处理方式都在", () => {
    render(
      <EditRecoveryDialog
        items={[item]}
        sessions={readySessions}
        onAction={vi.fn()}
        onLater={vi.fn()}
      />,
    );
    expect(screen.getByText("上次有 1 个文件的修改没有上传")).toBeTruthy();
    expect(screen.getByText("测试主机：/etc/app.conf")).toBeTruthy();
    expect(screen.getByText("重新上传")).toBeTruthy();
    expect(screen.getByText("另存到本地")).toBeTruthy();
    expect(screen.getByText("放弃")).toBeTruthy();
  });

  it("那台主机没有打开 SFTP 标签时不能重新上传，并说明原因", () => {
    render(
      <EditRecoveryDialog
        items={[item]}
        sessions={[]}
        onAction={vi.fn()}
        onLater={vi.fn()}
      />,
    );
    expect((screen.getByText("重新上传") as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(screen.getByText(/先打开「测试主机」的 SFTP 标签/)).toBeTruthy();
  });

  it("放弃要点两次确认", async () => {
    const onAction = vi.fn().mockResolvedValue({ state: "done" });
    render(
      <EditRecoveryDialog
        items={[item]}
        sessions={readySessions}
        onAction={onAction}
        onLater={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("放弃"));
    expect(onAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("确定放弃这份修改"));
    await waitFor(() => expect(onAction).toHaveBeenCalledWith(item, "discard"));
  });

  it("服务器上的文件变过时显示原因，并出现覆盖按钮", async () => {
    const onAction = vi.fn().mockResolvedValue({
      state: "conflict",
      message: "服务器上的文件在你离开之后又被改过，没有上传",
    });
    render(
      <EditRecoveryDialog
        items={[item]}
        sessions={readySessions}
        onAction={onAction}
        onLater={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("重新上传"));
    await waitFor(() =>
      expect(screen.getByText(/又被改过，没有上传/)).toBeTruthy(),
    );
    fireEvent.click(screen.getByText("覆盖服务器上的文件"));
    await waitFor(() =>
      expect(onAction).toHaveBeenLastCalledWith(item, "reupload_overwrite"),
    );
  });
});
