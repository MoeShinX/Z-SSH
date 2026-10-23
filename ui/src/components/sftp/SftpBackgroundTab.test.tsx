/** @vitest-environment jsdom */
//! 看不见的 SFTP 标签（后台标签）：传输进度只记下来不刷新界面，进行中的个数照常上报；
//! 切回前台一次显示最新状态。

import { act, render, waitFor } from "@testing-library/react";
import { beforeAll, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  transferHandlers: [] as Array<(id: string, transfer: Record<string, unknown>) => void>,
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: vi.fn(async (_id: string, path: string) => ({ path: path || "/home/u", entries: [] })),
    sftpLocalList: vi.fn(async () => []),
    sftpLocalDefaultDir: vi.fn(async () => "D:/home"),
    sftpTransfers: vi.fn(async () => []),
    onSftpTransfer: vi.fn(async (handler: (id: string, t: Record<string, unknown>) => void) => {
      mocks.transferHandlers.push(handler);
      return () => {};
    }),
    onSftpConflict: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpPasteProgress: vi.fn(async () => () => {}),
    onSftpArchiveProgress: vi.fn(async () => () => {}),
    onSftpEdit: vi.fn(async () => () => {}),
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import SftpTabView from "./SftpTabView";

beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

const running = {
  id: 7,
  direction: "upload",
  state: "running",
  source: "C:\\big.iso",
  target: "/tmp/big.iso",
  total_files: 1,
  done_files: 0,
  total_bytes: 1000,
  done_bytes: 300,
  speed_bps: 100,
  notes: [],
};

describe("后台 SFTP 标签", () => {
  it("看不见时不刷新队列、个数照常上报；切回来显示最新状态", async () => {
    const onActiveCountChange = vi.fn();
    const { container, rerender } = render(
      <SftpTabView sftpId="s1" active={false} onActiveCountChange={onActiveCountChange} />,
    );
    await waitFor(() => expect(mocks.transferHandlers.length).toBeGreaterThan(0));
    const push = (t: Record<string, unknown>) =>
      act(() => mocks.transferHandlers.forEach((handler) => handler("s1", t)));

    push(running);
    push({ ...running, done_bytes: 600 });
    expect(onActiveCountChange).toHaveBeenLastCalledWith(1);
    expect(container.textContent).not.toContain("big.iso");

    rerender(<SftpTabView sftpId="s1" active onActiveCountChange={onActiveCountChange} />);
    expect(container.textContent).toContain("big.iso");
    expect(container.textContent).toContain("60%");

    push({ ...running, state: "done", done_bytes: 1000 });
    expect(onActiveCountChange).toHaveBeenLastCalledWith(0);
  });
});
