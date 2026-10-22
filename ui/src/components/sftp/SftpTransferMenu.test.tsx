/** @vitest-environment jsdom */
//! 传输队列的右键菜单和「重试」（SFTP-6 第 4、5 节），接在真实的 `SftpTabView` 队列上：
//! - 一行右键弹菜单，项按状态显示，和右边的图标按钮一一对应；
//! - 「已跳过」也有重试，点了调 `sftp_transfer_retry`；
//! - 等同名文件的回答时状态写「等待处理同名文件」，不再一直「准备中」；
//! - 停靠面板和独立标签页的队列是同一个组件，表现一致。

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { SftpTransfer } from "../../api/sftp";

function transfer(id: number, state: SftpTransfer["state"], patch: Partial<SftpTransfer> = {}) {
  return {
    id,
    direction: "upload",
    state,
    source: `C:/work/file${id}.bin`,
    target: `/srv/file${id}.bin`,
    total_files: 1,
    done_files: 0,
    total_bytes: 1000,
    done_bytes: 0,
    speed_bps: 0,
    notes: [],
    ...patch,
  } as SftpTransfer;
}

const mocks = vi.hoisted(() => ({
  transfers: [] as unknown[],
  retry: vi.fn(async () => {}),
  pause: vi.fn(async () => {}),
  resume: vi.fn(async () => {}),
  cancel: vi.fn(async () => {}),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: vi.fn(async () => ({ path: "/home/u", entries: [] })),
    sftpLocalList: vi.fn(async () => []),
    sftpLocalDefaultDir: vi.fn(async () => "D:/home"),
    sftpTransfers: vi.fn(async () => mocks.transfers),
    sftpTransferRetry: mocks.retry,
    sftpTransferPause: mocks.pause,
    sftpTransferResume: mocks.resume,
    sftpTransferCancel: mocks.cancel,
    onSftpTransfer: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpPasteProgress: vi.fn(async () => () => {}),
    sftpPasteCancel: vi.fn(async () => {}),
    onSftpEdit: vi.fn(async () => () => {}),
  };
});

import SftpTabView from "./SftpTabView";

beforeAll(() => {
  class FakeResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
});

beforeEach(() => {
  mocks.retry.mockClear();
  mocks.pause.mockClear();
  mocks.resume.mockClear();
  mocks.cancel.mockClear();
});

afterEach(() => {
  cleanup();
});

async function mountQueue(transfers: SftpTransfer[], mode?: "dock") {
  mocks.transfers = transfers;
  const view = render(<SftpTabView sftpId="s1" mode={mode} />);
  await waitFor(() => expect(view.container.querySelector("[data-transfer-id]")).not.toBeNull());
  return view;
}

const row = (container: HTMLElement, id: number) =>
  container.querySelector(`[data-transfer-id="${id}"]`) as HTMLElement;

function menuLabels(): string[] {
  const menu = screen.queryByTestId("sftp-context-menu");
  if (!menu) return [];
  return within(menu)
    .getAllByRole("menuitem")
    .map((item) => item.textContent ?? "");
}

describe("队列项的右键菜单", () => {
  const cases: Array<[SftpTransfer["state"], string[]]> = [
    ["queued", ["暂停", "取消"]],
    ["planning", ["暂停", "取消"]],
    ["running", ["暂停", "取消"]],
    ["paused", ["继续", "取消"]],
    ["waiting_reconnect", ["取消"]],
    ["waiting_conflict", ["取消"]],
    ["failed", ["重试"]],
    ["cancelled", ["重试"]],
    ["skipped", ["重试"]],
  ];

  for (const [state, labels] of cases) {
    it(`状态 ${state}：菜单是 ${labels.join(" / ")}`, async () => {
      const { container } = await mountQueue([transfer(1, state)]);
      await act(async () => {
        fireEvent.contextMenu(row(container, 1), { clientX: 20, clientY: 20 });
      });
      expect(menuLabels()).toEqual(labels);
    });
  }

  it("已完成的行右键什么都不弹", async () => {
    const { container } = await mountQueue([transfer(1, "done", { done_bytes: 1000 })]);
    await act(async () => {
      fireEvent.contextMenu(row(container, 1), { clientX: 20, clientY: 20 });
    });
    expect(screen.queryByTestId("sftp-context-menu")).toBeNull();
  });

  it("已跳过的右键「重试」：调 sftp_transfer_retry，菜单关掉", async () => {
    const { container } = await mountQueue([transfer(5, "skipped")]);
    await act(async () => {
      fireEvent.contextMenu(row(container, 5), { clientX: 20, clientY: 20 });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: "重试" }));
    });
    expect(mocks.retry).toHaveBeenCalledWith("s1", 5);
    expect(screen.queryByTestId("sftp-context-menu")).toBeNull();
  });

  it("暂停 / 继续 / 取消各自调对应命令", async () => {
    const { container } = await mountQueue([
      transfer(1, "running"),
      transfer(2, "paused"),
    ]);
    const pick = async (id: number, name: string) => {
      await act(async () => {
        fireEvent.contextMenu(row(container, id), { clientX: 20, clientY: 20 });
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("menuitem", { name }));
      });
    };
    await pick(1, "暂停");
    expect(mocks.pause).toHaveBeenCalledWith("s1", 1);
    await pick(2, "继续");
    expect(mocks.resume).toHaveBeenCalledWith("s1", 2);
    await pick(1, "取消");
    expect(mocks.cancel).toHaveBeenCalledWith("s1", 1);
  });

  it("停靠面板里的队列一样有菜单", async () => {
    const { container } = await mountQueue([transfer(1, "failed")], "dock");
    await act(async () => {
      fireEvent.contextMenu(row(container, 1), { clientX: 20, clientY: 20 });
    });
    expect(menuLabels()).toEqual(["重试"]);
  });
});

describe("队列项右边的图标按钮", () => {
  it("已跳过也有「重试」按钮，点了调 sftp_transfer_retry", async () => {
    const { container } = await mountQueue([transfer(3, "skipped")]);
    const button = row(container, 3).querySelector('button[title="重试"]') as HTMLElement;
    expect(button).not.toBeNull();
    fireEvent.click(button);
    expect(mocks.retry).toHaveBeenCalledWith("s1", 3);
  });

  it("已完成的没有重试", async () => {
    const { container } = await mountQueue([transfer(3, "done")]);
    expect(row(container, 3).querySelector('button[title="重试"]')).toBeNull();
  });
});

describe("状态文字", () => {
  it("等同名文件的回答：写「等待处理同名文件」，可取消、不能暂停", async () => {
    const { container } = await mountQueue([transfer(1, "waiting_conflict")]);
    const line = row(container, 1);
    expect(line.textContent).toContain("等待处理同名文件");
    expect(line.textContent).not.toContain("准备中");
    expect(line.querySelector('button[title="取消"]')).not.toBeNull();
    expect(line.querySelector('button[title="暂停"]')).toBeNull();
  });

  it("准备中仍是准备中", async () => {
    const { container } = await mountQueue([transfer(1, "planning")]);
    expect(row(container, 1).textContent).toContain("准备中");
  });
});
