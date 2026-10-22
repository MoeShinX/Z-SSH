/** @vitest-environment jsdom */
//! 分组对话框：右键「未分组」→ 重命名 = 建一个分组并把未分组的主机都移进去。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import type { Host } from "../../api/vault";
import { useVault } from "../../stores/vault";
import GroupFormDialog from "./GroupFormDialog";

afterEach(cleanup);

const host = (id: number, group_id: number | null) => ({ id, group_id, name: `h${id}` }) as unknown as Host;

describe("重命名「未分组」", () => {
  it("建分组后把当时所有未分组的主机移进去", async () => {
    const saveGroup = vi.fn(async () => ({ id: 9, name: "生产", parent_id: null, sort: 0 }));
    const moveHosts = vi.fn(async () => {});
    useVault.setState({ groups: [], hosts: [host(1, null), host(2, 5), host(3, null)], saveGroup, moveHosts } as never);
    const onClose = vi.fn();
    render(<GroupFormDialog group={null} parentId={null} adoptUngrouped onClose={onClose} onError={() => {}} />);
    expect(screen.getByText("重命名「未分组」")).toBeTruthy();
    expect(screen.getByText(/2 台未分组的主机/)).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "生产" } });
    await act(async () => fireEvent.click(screen.getByText("保存")));
    expect(saveGroup).toHaveBeenCalledWith(expect.objectContaining({ id: null, name: "生产" }));
    expect(moveHosts).toHaveBeenCalledWith([1, 3], 9);
    expect(onClose).toHaveBeenCalled();
  });

  it("普通新建分组不移动主机", async () => {
    const saveGroup = vi.fn(async () => ({ id: 9, name: "测试", parent_id: null, sort: 0 }));
    const moveHosts = vi.fn(async () => {});
    useVault.setState({ groups: [], hosts: [host(1, null)], saveGroup, moveHosts } as never);
    render(<GroupFormDialog group={null} parentId={null} onClose={() => {}} onError={() => {}} />);
    expect(screen.getByText("新建分组")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "测试" } });
    await act(async () => fireEvent.click(screen.getByText("保存")));
    expect(moveHosts).not.toHaveBeenCalled();
  });
});
