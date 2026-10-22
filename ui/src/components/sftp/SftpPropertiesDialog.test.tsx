/** @vitest-environment jsdom */
//! 属性对话框：文件夹的「计算大小」可以取消，关掉对话框也会取消。
//! 同时算多个文件夹时，进度 / 结果 / 取消都要按发起时生成的关联 ID 各归各的。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SftpSizeEvent } from "../../api/sftp";

const mocks = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  handler: null as null | ((id: string, event: SftpSizeEvent) => void),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpSizeStart: mocks.start,
    sftpSizeCancel: mocks.cancel,
    onSftpSize: vi.fn(async (handler) => {
      mocks.handler = handler;
      return () => {
        mocks.handler = null;
      };
    }),
  };
});

import { SftpPropertiesDialog } from "./SftpPropertiesDialog";

const event = (patch: Partial<SftpSizeEvent>): SftpSizeEvent => ({
  request_id: "req-1",
  calc_id: 7,
  state: "running",
  files: 0,
  dirs: 0,
  bytes: 0,
  links: 0,
  unreadable: 0,
  ...patch,
});

const folder = { name: "logs", path: "/srv/logs", kind: "dir" as const, mtime: 1_700_000_000 };
const folderA = { name: "a", path: "/srv/a", kind: "dir" as const };
const folderB = { name: "b", path: "/srv/b", kind: "dir" as const };

beforeEach(() => {
  mocks.start.mockReset().mockResolvedValue(7);
  mocks.cancel.mockReset().mockResolvedValue(undefined);
  mocks.handler = null;
});
afterEach(cleanup);

const phase = () => document.querySelector('[data-prop="计算结果"]')?.getAttribute("data-phase");
const button = (action: string) => document.querySelector(`[data-action="${action}"]`) as HTMLElement;
/** 对话框发起计算时生成的关联 ID（`sftpSizeStart` 的第四个参数）。 */
const startedRequestIds = () => mocks.start.mock.calls.map((call) => call[3] as string | undefined);

describe("SftpPropertiesDialog", () => {
  it("文件：直接显示大小和权限，没有计算按钮", () => {
    render(
      <SftpPropertiesDialog
        sftpId="s1"
        side="remote"
        targets={[
          { name: "a.conf", path: "/srv/a.conf", kind: "file", size: 2048, permissions: 0o100644, owner: "root" },
        ]}
        onClose={() => {}}
      />,
    );
    expect(document.querySelector('[data-prop="大小"]')?.textContent).toBe("2.0 KB（2,048 字节）");
    expect(document.querySelector('[data-prop="权限"]')?.textContent).toBe("0644");
    expect(button("size-start")).toBeNull();
  });

  it("文件夹：点计算 → 进度 → 完成；完成后按钮变成重新计算", async () => {
    render(<SftpPropertiesDialog sftpId="s1" side="remote" targets={[folder]} onClose={() => {}} />);
    expect(phase()).toBe("idle");
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    expect(mocks.start).toHaveBeenCalledWith("s1", "remote", "/srv/logs", expect.any(String));
    const requestId = startedRequestIds()[0];
    expect(requestId).toEqual(expect.any(String));
    await waitFor(() => expect(phase()).toBe("running"));
    expect(button("size-cancel")).not.toBeNull();
    await act(async () => {
      mocks.handler?.("s1", event({ request_id: requestId!, files: 4, dirs: 2, bytes: 4096 }));
    });
    expect(screen.getByRole("status").textContent).toContain("4 个文件");
    await act(async () => {
      mocks.handler?.("s1", event({ request_id: requestId!, state: "done", files: 9, dirs: 3, bytes: 9000 }));
    });
    expect(phase()).toBe("done");
    expect(button("size-start").textContent).toBe("重新计算");
  });

  it("计算中点「取消计算」：通知后端，收到取消事件后显示不完整的结果", async () => {
    render(<SftpPropertiesDialog sftpId="s1" side="remote" targets={[folder]} onClose={() => {}} />);
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    await waitFor(() => expect(phase()).toBe("running"));
    await act(async () => {
      fireEvent.click(button("size-cancel"));
    });
    await waitFor(() => expect(mocks.cancel).toHaveBeenCalledWith("s1", 7));
    await act(async () => {
      mocks.handler?.("s1", event({ request_id: startedRequestIds()[0]!, state: "cancelled", files: 3, bytes: 300 }));
    });
    expect(phase()).toBe("cancelled");
    expect(screen.getByRole("status").textContent).toContain("不是完整结果");
  });

  it("计算中直接关对话框：也要取消后端的计算", async () => {
    const view = render(
      <SftpPropertiesDialog sftpId="s1" side="remote" targets={[folder]} onClose={() => {}} />,
    );
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    await waitFor(() => expect(phase()).toBe("running"));
    view.unmount();
    expect(mocks.cancel).toHaveBeenCalledWith("s1", 7);
  });

  it("计算命令本身失败：显示原因", async () => {
    mocks.start.mockRejectedValue("SFTP 标签不存在");
    render(<SftpPropertiesDialog sftpId="s1" side="remote" targets={[folder]} onClose={() => {}} />);
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    await waitFor(() => expect(phase()).toBe("failed"));
    expect(screen.getByRole("status").textContent).toContain("SFTP 标签不存在");
  });

  it("多选：显示合计，计算只数文件夹", async () => {
    render(
      <SftpPropertiesDialog
        sftpId="s1"
        side="local"
        targets={[folder, { name: "a.txt", path: "/srv/a.txt", kind: "file", size: 1000 }]}
        onClose={() => {}}
      />,
    );
    expect(document.querySelector('[data-prop="选中"]')?.textContent).toContain("2 项");
    expect(document.querySelector('[data-prop="文件大小"]')?.textContent).toContain("1000");
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    expect(mocks.start).toHaveBeenCalledTimes(1);
  });

  it("两个文件夹同时算：事件比命令返回值先到也不会串号，结果各归各的", async () => {
    let resolveA!: (value: number) => void;
    let resolveB!: (value: number) => void;
    mocks.start.mockImplementation((_sftpId: string, _side: string, path: string) => {
      if (path === "/srv/a") {
        return new Promise<number>((resolve) => {
          resolveA = resolve;
        });
      }
      return new Promise<number>((resolve) => {
        resolveB = resolve;
      });
    });
    render(<SftpPropertiesDialog sftpId="s1" side="remote" targets={[folderA, folderB]} onClose={() => {}} />);
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    expect(mocks.start).toHaveBeenCalledTimes(2);
    const requests = startedRequestIds();
    expect(requests[0]).toEqual(expect.any(String));
    expect(requests[1]).toEqual(expect.any(String));
    expect(requests[0]).not.toBe(requests[1]);

    // 关键：进度事件比 sftp_size_start 的返回值先到。A 的事件不能被 B 认领。
    await act(async () => {
      mocks.handler?.("s1", event({ request_id: requests[0]!, calc_id: 11, files: 1, dirs: 2, bytes: 100 }));
      mocks.handler?.("s1", event({ request_id: requests[1]!, calc_id: 12, files: 2, dirs: 2, bytes: 200 }));
    });
    expect(screen.getByRole("status").textContent).toContain("300 字节");

    await act(async () => {
      resolveA(11);
      resolveB(12);
    });
    // 命令返回值到齐之后也不能把已经收到的结果冲掉：合计依然是 A + B。
    expect(screen.getByRole("status").textContent).toContain("300 字节");
    expect(screen.getByRole("status").textContent).toContain("3 个文件");
  });

  it("两个文件夹同时算：取消各取消各的（不会都取消成第一个的任务号）", async () => {
    let resolveA!: (value: number) => void;
    let resolveB!: (value: number) => void;
    mocks.start.mockImplementation((_sftpId: string, _side: string, path: string) => {
      if (path === "/srv/a") {
        return new Promise<number>((resolve) => {
          resolveA = resolve;
        });
      }
      return new Promise<number>((resolve) => {
        resolveB = resolve;
      });
    });
    render(<SftpPropertiesDialog sftpId="s1" side="remote" targets={[folderA, folderB]} onClose={() => {}} />);
    await act(async () => {
      fireEvent.click(button("size-start"));
    });
    const requests = startedRequestIds();
    await act(async () => {
      mocks.handler?.("s1", event({ request_id: requests[0]!, calc_id: 11, files: 1, bytes: 10 }));
      mocks.handler?.("s1", event({ request_id: requests[1]!, calc_id: 12, files: 1, bytes: 20 }));
    });
    await act(async () => {
      resolveA(11);
      resolveB(12);
    });
    await act(async () => {
      fireEvent.click(button("size-cancel"));
    });
    expect(mocks.cancel).toHaveBeenCalledTimes(2);
    const cancelled = mocks.cancel.mock.calls.map((call) => call[1]);
    expect(new Set(cancelled)).toEqual(new Set([11, 12]));
  });
});
