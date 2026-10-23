/** @vitest-environment jsdom */
//! 停靠面板的本地栏可以收起（SFTP-6 第 1 节）：
//! - 本地栏工具条最右边有 ×（提示「关闭本地栏」），点了只剩远端栏，状态写进设置并在重新渲染后保持；
//! - 面板标题栏的「本地」按钮把它请回来；
//! - 只影响停靠面板：独立标签页 / 文件窗口的双栏不显示 ×，也不受设置影响；
//! - 收起后本地目录仍在后台列着，资源管理器拖进来上传等照常工作。

import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  sftpList: vi.fn(async (_id: string, path: string) => ({
    path: path || "/home/u",
    entries: [] as unknown[],
  })),
  sftpLocalList: vi.fn(async () => [] as unknown[]),
  sftpTransferSubmit: vi.fn(async () => [1]),
  settingsSet: vi.fn(async () => {}),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: mocks.sftpList,
    sftpLocalList: mocks.sftpLocalList,
    sftpLocalDefaultDir: vi.fn(async () => "D:/home"),
    sftpTransfers: vi.fn(async () => []),
    sftpTransferSubmit: mocks.sftpTransferSubmit,
    onSftpTransfer: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpPasteProgress: vi.fn(async () => () => {}),
    onSftpArchiveProgress: vi.fn(async () => () => {}),
    sftpPasteCancel: vi.fn(async () => {}),
    onSftpEdit: vi.fn(async () => () => {}),
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
}));

import { handleExternalDragEvent } from "../../state/pointerDrag";
import { useSettings } from "../../stores/settings";
import SftpDockPanel from "./SftpDockPanel";
import SftpTabView from "./SftpTabView";

const KEY = "layout.sftp_dock_local_hidden";

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
  mocks.sftpTransferSubmit.mockClear();
  mocks.settingsSet.mockClear();
  useSettings.setState({ [KEY]: false } as never);
});

afterEach(() => {
  cleanup();
});

const sides = (container: HTMLElement) =>
  Array.from(container.querySelectorAll("[data-pane-side]")).map((el) =>
    el.getAttribute("data-pane-side"),
  );

describe("本地栏的 ×", () => {
  it("默认两栏都在；本地栏最右边有「关闭本地栏」，远端栏没有", async () => {
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    expect(sides(container)).toEqual(["remote", "local"]);
    const local = container.querySelector('[data-pane-side="local"]') as HTMLElement;
    const remote = container.querySelector('[data-pane-side="remote"]') as HTMLElement;
    const close = local.querySelector('button[title="关闭本地栏"]') as HTMLElement;
    expect(close).not.toBeNull();
    expect(remote.querySelector('button[title="关闭本地栏"]')).toBeNull();
    // 在「过滤」输入框的右边（工具条最后一个）
    const filter = local.querySelector('input[placeholder="过滤"]') as HTMLElement;
    expect(
      filter.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("点 ×：本地栏收起，只剩远端栏占满高度，没有分隔线；状态写进设置", async () => {
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    await act(async () => {
      fireEvent.click(container.querySelector('button[title="关闭本地栏"]') as HTMLElement);
    });
    expect(mocks.settingsSet).toHaveBeenCalledWith(KEY, "true");
    expect(sides(container)).toEqual(["remote"]);
    expect(container.querySelector('[data-section="sftp-split-handle"]')).toBeNull();
    const remoteWrap = (container.querySelector('[data-pane-side="remote"]') as HTMLElement)
      .parentElement as HTMLElement;
    expect(remoteWrap.style.flex).toBe("1 1 0%");
    // 传输队列还在
    expect(container.textContent).toContain("传输队列");
  });

  it("设置里已经是收起：重新渲染（换主机、重开面板）后保持收起", async () => {
    useSettings.setState({ [KEY]: true } as never);
    const first = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    expect(sides(first.container)).toEqual(["remote"]);
    first.unmount();
    const second = render(<SftpTabView sftpId="s2" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalledWith("s2", ""));
    expect(sides(second.container)).toEqual(["remote"]);
  });

  it("设置改回 false：本地栏回来，仍在分隔线下面", async () => {
    useSettings.setState({ [KEY]: true } as never);
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    await act(async () => {
      useSettings.setState({ [KEY]: false } as never);
    });
    expect(sides(container)).toEqual(["remote", "local"]);
    expect(container.querySelector('[data-section="sftp-split-handle"]')).not.toBeNull();
  });

  it("独立标签页：不显示 ×，设置为收起也照样双栏", async () => {
    useSettings.setState({ [KEY]: true } as never);
    const { container } = render(<SftpTabView sftpId="s1" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    expect(sides(container)).toEqual(["local", "remote"]);
    expect(container.querySelector('button[title="关闭本地栏"]')).toBeNull();
  });
});

describe("收起后不依赖本地栏界面的功能", () => {
  it("本地目录仍在后台列着（下载的目标目录照旧）", async () => {
    useSettings.setState({ [KEY]: true } as never);
    render(<SftpTabView sftpId="s1" mode="dock" initialPaths={{ local: "D:/work", remote: "" }} />);
    await waitFor(() => expect(mocks.sftpLocalList).toHaveBeenCalledWith("D:/work"));
  });

  it("从资源管理器拖进远端栏：照常上传到远端当前目录", async () => {
    useSettings.setState({ [KEY]: true } as never);
    const { container } = render(<SftpTabView sftpId="s1" mode="dock" />);
    await waitFor(() => expect(mocks.sftpList).toHaveBeenCalled());
    await waitFor(() => expect(container.querySelector('[data-pane-side="remote"]')).not.toBeNull());
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
});

describe("面板标题栏的「本地」按钮", () => {
  it("收起时才有：提示「显示本地栏」，点了调用恢复", () => {
    const onShowLocal = vi.fn();
    const { container, rerender } = render(
      <SftpDockPanel
        hostLabel="web-1"
        widthOverride={null}
        reservedWidth={0}
        onClose={() => {}}
        onShowLocal={onShowLocal}
      >
        <div />
      </SftpDockPanel>,
    );
    const button = container.querySelector('button[title="显示本地栏"]') as HTMLElement;
    expect(button).not.toBeNull();
    expect(button.textContent).toContain("本地");
    fireEvent.click(button);
    expect(onShowLocal).toHaveBeenCalledTimes(1);

    rerender(
      <SftpDockPanel hostLabel="web-1" widthOverride={null} reservedWidth={0} onClose={() => {}}>
        <div />
      </SftpDockPanel>,
    );
    expect(container.querySelector('button[title="显示本地栏"]')).toBeNull();
  });
});
