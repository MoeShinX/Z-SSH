/** @vitest-environment jsdom */
//! SFTP 停靠面板的组件测试（M3-3 第 6.1 节）：
//! - dock 模式：远端在上、本地在下；
//! - 停靠 / 拆回时两栏目录保留（initialPaths 恢复、onPathsChange 上报）；
//! - 跟随终端目录：followPath 变了远端栏跟过去；用户手动换目录触发 onManualRemoteNavigate，
//!   程序化的跟随不触发；
//! - 上下比例从设置读，分隔线双击写回 0.5。

import { act, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  sftpList: vi.fn(async (_id: string, path: string) => ({
    path: path || "/home/u",
    entries: [] as unknown[],
  })),
  sftpLocalList: vi.fn(async () => [] as unknown[]),
  sftpTransferSubmit: vi.fn(async () => 1),
  sftpLocalDefaultDir: vi.fn(async () => "D:/home"),
  sftpTransfers: vi.fn(async () => [] as unknown[]),
  settingsSet: vi.fn(async () => {}),
  /** onSftpEdit 注册的处理函数：测试里手动推事件。 */
  editHandlers: [] as Array<(event: Record<string, unknown>) => void>,
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: mocks.sftpList,
    sftpLocalList: mocks.sftpLocalList,
    sftpLocalDefaultDir: mocks.sftpLocalDefaultDir,
    sftpTransfers: mocks.sftpTransfers,
    sftpTransferSubmit: mocks.sftpTransferSubmit,
    onSftpTransfer: vi.fn(async () => () => {}),
    onSftpConflict: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpPasteProgress: vi.fn(async () => () => {}),
    onSftpArchiveProgress: vi.fn(async () => () => {}),
    sftpPasteCancel: vi.fn(async () => {}),
    onSftpEdit: vi.fn(async (handler: (event: Record<string, unknown>) => void) => {
      mocks.editHandlers.push(handler);
      return () => {};
    }),
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
}));

import { useSettings } from "../../stores/settings";
import { handleExternalDragEvent } from "../../state/pointerDrag";
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
  mocks.sftpList.mockClear();
  mocks.sftpLocalList.mockClear();
  mocks.settingsSet.mockClear();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("dock 模式布局", () => {
  it("远端栏在上、本地栏在下", async () => {
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    const sides = Array.from(container.querySelectorAll("[data-pane-side]")).map((el) =>
      el.getAttribute("data-pane-side"),
    );
    expect(sides).toEqual(["remote", "local"]);
  });

  it("独立标签模式：本地在左、远端在右（M3-2 不变）", async () => {
    const { container } = render(<SftpTabView sftpId="s1" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    const sides = Array.from(container.querySelectorAll("[data-pane-side]")).map((el) =>
      el.getAttribute("data-pane-side"),
    );
    expect(sides).toEqual(["local", "remote"]);
  });

  it("上下比例读设置表；双击分隔线写回 0.5", async () => {
    useSettings.setState({ "layout.sftp_split_ratio": 0.3 });
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    const panes = container.querySelectorAll("[data-pane-side]");
    const topWrap = panes[0].parentElement as HTMLElement;
    expect(topWrap.style.flex).toContain("0.3");
    const handle = container.querySelector('[data-section="sftp-split-handle"]') as HTMLElement;
    await act(async () => {
      fireEvent.doubleClick(handle);
    });
    expect(mocks.settingsSet).toHaveBeenCalledWith("layout.sftp_split_ratio", "0.5");
  });
});

describe("两栏目录保留与跟随终端目录", () => {
  it("initialPaths 带过来的目录被恢复（停靠 / 拆回）", async () => {
    render(
      <SftpTabView
        sftpId="s1"
        mode="dock"
        initialPaths={{ local: "D:/work", remote: "/srv/app" }}
      />,
    );
    await waitFor(() => {
      expect(mocks.sftpLocalList).toHaveBeenCalledWith("D:/work");
      expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/srv/app");
    });
  });

  it("目录变化通过 onPathsChange 上报，带的是两栏当前值", async () => {
    const onPathsChange = vi.fn();
    render(
      <SftpTabView
        sftpId="s1"
        initialPaths={{ local: "D:/work", remote: "/srv/app" }}
        onPathsChange={onPathsChange}
      />,
    );
    await waitFor(() =>
      expect(onPathsChange).toHaveBeenLastCalledWith({ local: "D:/work", remote: "/srv/app" }),
    );
  });

  it("followPath 变了：远端栏跟过去，且不算用户手动换目录", async () => {
    const onManual = vi.fn();
    const { rerender } = render(
      <SftpTabView sftpId="s1" mode="dock" followPath="/a" onManualRemoteNavigate={onManual} />,
    );
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/a"));
    mocks.sftpList.mockClear();
    rerender(
      <SftpTabView sftpId="s1" mode="dock" followPath="/b" onManualRemoteNavigate={onManual} />,
    );
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/b"));
    expect(onManual).not.toHaveBeenCalled();
  });

  it("followPath 为空（已停止跟随）：终端目录再怎么变，远端栏都不动", async () => {
    const { rerender } = render(<SftpTabView sftpId="s1" mode="dock" followPath={null} />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    mocks.sftpList.mockClear();
    rerender(<SftpTabView sftpId="s1" mode="dock" followPath={null} />);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(mocks.sftpList).not.toHaveBeenCalled();
  });

  it("带过来的远端目录没了：退回家目录，不报错", async () => {
    mocks.sftpList.mockImplementationOnce(async () => {
      throw new Error("no such directory");
    });
    const { container } = render(
      <SftpTabView sftpId="s1" mode="dock" initialPaths={{ local: "", remote: "/gone" }} />,
    );
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", ""));
    expect(container.textContent ?? "").not.toContain("no such directory");
  });

  it("用户在远端栏点面包屑 / 回上级：触发 onManualRemoteNavigate", async () => {
    const onManual = vi.fn();
    const { container } = render(
      <SftpTabView
        sftpId="s1"
        mode="dock"
        initialPaths={{ local: "", remote: "/srv/app" }}
        onManualRemoteNavigate={onManual}
      />,
    );
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/srv/app"));
    await waitFor(() => {
      const remote = container.querySelector('[data-pane-side="remote"]');
      expect(remote?.querySelector('button[title^="返回上级"]')).not.toBeNull();
    });
    const remote = container.querySelector('[data-pane-side="remote"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(remote.querySelector('button[title^="返回上级"]') as HTMLElement);
    });
    expect(onManual).toHaveBeenCalledTimes(1);
  });
});

describe("两栏之间拖动 = 传输（指针拖动）", () => {
  it("本地栏的文件拖到远端栏：提交上传，目标是远端当前目录；拖到目录行上就是那个目录", async () => {
    mocks.sftpLocalList.mockImplementation(async () => [
      { name: "a.txt", path: "D:/work/a.txt", kind: "file", name_is_utf8: true, hidden: false, size: 3 },
    ]);
    mocks.sftpList.mockImplementation(async (_id: string, path: string) => ({
      path: path || "/home/u",
      entries: [
        { name: "logs", path: "/home/u/logs", kind: "dir", name_is_utf8: true, hidden: false },
      ],
    }));
    const { container } = render(<SftpTabView sftpId="s1" />);
    await waitFor(() => {
      expect(container.querySelector('[data-pane-side="local"] [data-row]')).not.toBeNull();
      expect(container.querySelector('[data-pane-side="remote"] [data-row]')).not.toBeNull();
    });
    const localRow = container.querySelector('[data-pane-side="local"] [data-row]') as HTMLElement;
    const remote = container.querySelector('[data-pane-side="remote"]') as HTMLElement;
    const remoteRow = remote.querySelector("[data-row]") as HTMLElement;

    // 落在远端栏的空白处：当前目录。
    fireEvent.pointerDown(localRow, { button: 0, clientX: 5, clientY: 5 });
    document.elementFromPoint = () => remote;
    act(() => {
      window.dispatchEvent(new MouseEvent("pointermove", { clientX: 60, clientY: 60, bubbles: true }));
      window.dispatchEvent(new MouseEvent("pointerup", { clientX: 60, clientY: 60, bubbles: true }));
    });
    expect(mocks.sftpTransferSubmit).toHaveBeenLastCalledWith(
      "s1",
      "upload",
      ["D:/work/a.txt"],
      "/home/u",
    );

    // 落在目录行上：目标是那个目录。
    fireEvent.pointerDown(localRow, { button: 0, clientX: 5, clientY: 5 });
    document.elementFromPoint = () => remoteRow;
    act(() => {
      window.dispatchEvent(new MouseEvent("pointermove", { clientX: 60, clientY: 60, bubbles: true }));
      window.dispatchEvent(new MouseEvent("pointerup", { clientX: 60, clientY: 60, bubbles: true }));
    });
    expect(mocks.sftpTransferSubmit).toHaveBeenLastCalledWith(
      "s1",
      "upload",
      ["D:/work/a.txt"],
      "/home/u/logs",
    );
  });
});

describe("资源管理器拖入上传", () => {
  const entriesFor = async () => {
    mocks.sftpLocalList.mockImplementation(async () => []);
    mocks.sftpList.mockImplementation(async (_id: string, path: string) => ({
      path: path || "/home/u",
      entries: [
        { name: "logs", path: "/home/u/logs", kind: "dir", name_is_utf8: true, hidden: false },
      ],
    }));
  };

  it("拖到远端栏空白处：上传到当前目录，走同一个传输队列", async () => {
    await entriesFor();
    mocks.sftpTransferSubmit.mockClear();
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() =>
      expect(container.querySelector('[data-pane-side="remote"] [data-row]')).not.toBeNull(),
    );
    const remote = container.querySelector('[data-pane-side="remote"]') as HTMLElement;
    document.elementFromPoint = () => remote;
    act(() => {
      handleExternalDragEvent(
        { type: "drop", paths: ["D:/work/a.txt"], position: { x: 10, y: 10 } },
        1,
      );
    });
    expect(mocks.sftpTransferSubmit).toHaveBeenCalledWith(
      "s1",
      "upload",
      ["D:/work/a.txt"],
      "/home/u",
    );
  });

  it("拖到远端栏的目录行上：上传到那个目录", async () => {
    await entriesFor();
    mocks.sftpTransferSubmit.mockClear();
    const { container } = render(<SftpTabView sftpId="s1" />);
    await waitFor(() =>
      expect(container.querySelector('[data-pane-side="remote"] [data-row]')).not.toBeNull(),
    );
    const row = container.querySelector('[data-pane-side="remote"] [data-row]') as HTMLElement;
    document.elementFromPoint = () => row;
    act(() => {
      handleExternalDragEvent(
        { type: "drop", paths: ["D:/work/a.txt"], position: { x: 10, y: 10 } },
        1,
      );
    });
    expect(mocks.sftpTransferSubmit).toHaveBeenCalledWith(
      "s1",
      "upload",
      ["D:/work/a.txt"],
      "/home/u/logs",
    );
  });

  it("拖到本地栏：不做任何事", async () => {
    await entriesFor();
    mocks.sftpTransferSubmit.mockClear();
    const { container } = render(<SftpTabView sftpId="s1" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    const local = container.querySelector('[data-pane-side="local"]') as HTMLElement;
    document.elementFromPoint = () => local;
    act(() => {
      handleExternalDragEvent(
        { type: "drop", paths: ["D:/work/a.txt"], position: { x: 10, y: 10 } },
        1,
      );
    });
    expect(mocks.sftpTransferSubmit).not.toHaveBeenCalled();
  });
});

describe("编辑器保存完成后刷新远端栏（M3-4 第 0 节）", () => {
  const uploaded = (patch: Record<string, unknown> = {}) => ({
    sftp_id: "s1",
    edit_id: 1,
    state: "uploaded",
    remote_path: "/srv/app/edit-me.txt",
    remote_gone: false,
    suggested_name: "",
    dirty: false,
    ...patch,
  });

  for (const mode of ["dock", "tab"] as const) {
    it(`${mode === "dock" ? "停靠面板" : "独立 SFTP 标签"}：文件所在目录就是当前目录时重新列一遍`, async () => {
      mocks.editHandlers.length = 0;
      render(
        <SftpTabView
          sftpId="s1"
          mode={mode === "dock" ? "dock" : undefined}
          initialPaths={{ local: "D:/work", remote: "/srv/app" }}
        />,
      );
      await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/srv/app"));
      await waitFor(() => expect(mocks.editHandlers.length).toBeGreaterThan(0));
      mocks.sftpList.mockClear();
      // 另一个目录、另一台会话：不刷新。
      await act(async () => {
        mocks.editHandlers.forEach((h) => h(uploaded({ remote_path: "/other/x.txt" })));
        mocks.editHandlers.forEach((h) => h(uploaded({ sftp_id: "s9" })));
      });
      expect(mocks.sftpList).not.toHaveBeenCalled();
      // 另存为之后的新文件也在这个目录：刷新。
      await act(async () => {
        mocks.editHandlers.forEach((h) =>
          h(uploaded({ remote_path: "/srv/app/edit-me (副本).txt" })),
        );
      });
      await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s1", "/srv/app"));
    });
  }
});
