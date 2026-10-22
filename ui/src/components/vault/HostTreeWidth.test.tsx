/** @vitest-environment jsdom */
//! 主机树宽度（UI-5）：主机页和终端界面读同一个设置值，非法值回退 224；
//! 拖右边缘时侧栏和顶栏左侧块实时一起变，松手才保存。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: vi.fn(async () => false),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(),
  }),
}));
vi.mock("../../api/vault", async (importOriginal) => ({
  draftOfHost: (await importOriginal<typeof import("../../api/vault")>()).draftOfHost,
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import { resetPanelDraggingForTests } from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import TopTabs from "../titlebar/TopTabs";
import VaultView, { HostTreeAside } from "./VaultView";

const noop = () => {};

function asideProps() {
  return {
    query: "",
    selectedGroup: "all" as const,
    onSelectGroup: noop,
    selectedHostId: null,
    hostActions: {
      select: noop,
      connect: noop,
      openSftp: noop,
      remove: noop,
      copy: noop,
      installKey: noop,
      forwards: noop,
    },
    onNewHost: noop,
    onNewGroup: noop,
    onEditGroup: noop,
    onDeleteGroup: noop,
  };
}

/** 和 App 终端界面一样的接法：顶栏 + 主机树（collapsedStyle="hidden"）。 */
function TerminalWithTree() {
  return (
    <>
      <TopTabs
        tabs={[]}
        activeTabId=""
        panes={{}}
        view="terminal"
        onShowVault={noop}
        onOpenSettings={noop}
        onSelectTab={noop}
        onCloseTab={noop}
        onNewTab={noop}
        aiOpen={false}
        onToggleAI={noop}
        sftpOpen={false}
        onToggleSftp={noop}
      />
      <HostTreeAside {...asideProps()} collapsedStyle="hidden" />
    </>
  );
}

const aside = () => document.querySelector<HTMLElement>('[data-section="vault-sidebar"]');
const leftBlock = () => document.querySelector<HTMLElement>('[data-section="top-tabs-left"]');

afterEach(() => {
  cleanup();
  resetPanelDraggingForTests();
});

beforeEach(() => {
  useVault.setState({ groups: [], hosts: [], load: async () => {} } as never);
  useSettings.setState({
    "layout.vault_expanded": "",
    "layout.vault_sidebar_collapsed": false,
    "layout.host_tree_width": 224,
  });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("主机树宽度", () => {
  it("默认 224；非法值回退 224", () => {
    render(<HostTreeAside {...asideProps()} collapsedStyle="hidden" />);
    expect(aside()?.style.width).toBe("224px");
    cleanup();

    for (const bad of [-1, 0, Number.NaN, "x", null, 9_999_999]) {
      useSettings.setState({ "layout.host_tree_width": bad as never });
      render(<HostTreeAside {...asideProps()} collapsedStyle="hidden" />);
      expect(aside()?.style.width).toBe("224px");
      cleanup();
    }
  });

  it("主机页和终端界面读同一个值", () => {
    useSettings.setState({ "layout.host_tree_width": 300 });
    render(
      <VaultView
        onOpenHost={noop}
        onOpenSftp={noop}
        onQuickConnect={noop}
        onSelectHost={noop}
        onNewHost={noop}
        onNewGroup={noop}
        onEditGroup={noop}
        onDeleteGroup={noop}
        onDeleteHost={noop}
        onOpenIdentities={noop}
        onOpenProxies={noop}
        onOpenForwards={noop}
        onNotice={noop}
      />,
    );
    const vaultWidth = aside()?.style.width;
    cleanup();
    render(<HostTreeAside {...asideProps()} collapsedStyle="hidden" />);
    expect(vaultWidth).toBe("300px");
    expect(aside()?.style.width).toBe("300px");
  });

  it("拖右边缘：侧栏和顶栏左侧块实时一起变，松手才保存，双击恢复 224", async () => {
    render(<TerminalWithTree />);
    expect(aside()?.style.width).toBe("224px");
    expect(leftBlock()?.style.width).toBe("224px");

    const handle = screen.getByRole("separator");
    fireEvent.pointerDown(handle, { clientX: 224, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 284, pointerId: 1 });
    expect(aside()?.style.width).toBe("284px");
    expect(leftBlock()?.style.width).toBe("284px");
    expect(useSettings.getState()["layout.host_tree_width"]).toBe(224); // 拖动中不写

    await act(async () => {
      fireEvent.pointerUp(handle, { clientX: 284, pointerId: 1 });
    });
    expect(useSettings.getState()["layout.host_tree_width"]).toBe(284);
    expect(aside()?.style.width).toBe("284px");
    expect(leftBlock()?.style.width).toBe("284px");

    await act(async () => {
      fireEvent.doubleClick(handle);
    });
    expect(useSettings.getState()["layout.host_tree_width"]).toBe(224);
    expect(aside()?.style.width).toBe("224px");
    expect(leftBlock()?.style.width).toBe("224px");
  });

  it("收起后侧栏不渲染，顶栏左侧块不再固定宽度", () => {
    useSettings.setState({ "layout.vault_sidebar_collapsed": true, "layout.host_tree_width": 300 });
    render(<TerminalWithTree />);
    expect(aside()).toBeNull();
    expect(leftBlock()?.style.width).toBe("");
  });
});
