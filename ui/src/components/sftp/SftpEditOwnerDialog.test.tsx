/** @vitest-environment jsdom */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { SftpEditEvent } from "../../api/sftp";
import { SftpEditOwnerDialog } from "./SftpEditOwnerDialog";

afterEach(cleanup);

const event: SftpEditEvent = {
  sftp_id: "s1",
  edit_id: 4,
  remote_path: "/etc/app.conf",
  state: "owner_prompt",
  remote_gone: false,
  suggested_name: "app (副本).conf",
  dirty: true,
  owner: {
    from: "uid 1001 / gid 1002",
    to: "uid 1000 / gid 1000",
    error: "权限不够",
  },
};

describe("SftpEditOwnerDialog", () => {
  it("写清楚属主从谁变成谁，给三个选项", () => {
    render(
      <SftpEditOwnerDialog
        event={event}
        onChoose={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText("保存后文件的属主会改变")).toBeTruthy();
    const text = document.querySelector("[data-owner-text]")?.textContent ?? "";
    expect(text).toContain("uid 1001 / gid 1002");
    expect(text).toContain("uid 1000 / gid 1000");
    expect(
      screen.getByRole("button", { name: "仍然保存（属主改变）" }),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "直接写入原文件" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "取消" })).toBeTruthy();
    // 直接写入的风险要说明白。
    expect(document.body.textContent).toContain("半截");
  });

  it("每个选项各自调用对应的回调", () => {
    const onChoose = vi.fn();
    const onCancel = vi.fn();
    render(
      <SftpEditOwnerDialog
        event={event}
        onChoose={onChoose}
        onCancel={onCancel}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "仍然保存（属主改变）" }),
    );
    expect(onChoose).toHaveBeenLastCalledWith("owner_keep_new");
    fireEvent.click(screen.getByRole("button", { name: "直接写入原文件" }));
    expect(onChoose).toHaveBeenLastCalledWith("owner_in_place");
    expect(onCancel).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});
