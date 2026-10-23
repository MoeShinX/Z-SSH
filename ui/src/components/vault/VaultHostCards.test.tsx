/** @vitest-environment jsdom */
//! 主机页右边的卡片 / 列表（UI-10）：单击只选中，双击连接，右键「编辑」和编辑按钮才打开详情。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

import type { Host } from "../../api/vault";
import { useSettings } from "../../stores/settings";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useVault } from "../../stores/vault";
import { UNGROUPED_KEY } from "../../state/hostTree";
import VaultView from "./VaultView";

const host = (id: number, name: string) =>
  ({
    id,
    group_id: null,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags: [],
  }) as unknown as Host;

const props = () => ({
  onOpenHost: vi.fn(),
  onOpenSftp: vi.fn(),
  onQuickConnect: vi.fn(),
  onSelectHost: vi.fn(),
  onNewHost: vi.fn(),
  onNewGroup: vi.fn(),
  onEditGroup: vi.fn(),
  onDeleteGroup: vi.fn(),
  onDeleteHost: vi.fn(),
  onOpenIdentities: vi.fn(),
  onOpenProxies: vi.fn(),
  onOpenForwards: vi.fn(),
  onNotice: vi.fn(),
});

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  useVault.setState({
    groups: [],
    hosts: [host(1, "alpha"), host(2, "beta")],
    load: async () => {},
  } as never);
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

function setMode(mode: "grid" | "list") {
  useSettings.setState({
    "layout.vault_expanded": "",
    "layout.vault_sidebar_collapsed": false,
    "layout.vault_view_mode": mode,
  });
}

const cases = [
  { mode: "grid", section: "host-card" },
  { mode: "list", section: "host-row" },
] as const;

for (const { mode, section } of cases) {
  describe(`主机页${mode === "grid" ? "卡片" : "列表"}的点击`, () => {
    const item = (id: number) => document.querySelector(`[data-section="${section}"][data-host-id="${id}"]`) as HTMLElement;

    beforeEach(() => setMode(mode));

    it("单击只选中：不调用 onSelectHost，有选中样式，换一台时选中跟着走", () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      expect(item(1).getAttribute("data-selected")).toBeNull();
      fireEvent.click(item(1));
      expect(handlers.onSelectHost).not.toHaveBeenCalled();
      expect(handlers.onOpenHost).not.toHaveBeenCalled();
      expect(item(1).getAttribute("data-selected")).toBe("true");
      expect(item(2).getAttribute("data-selected")).toBeNull();
      fireEvent.click(item(2));
      expect(item(1).getAttribute("data-selected")).toBeNull();
      expect(item(2).getAttribute("data-selected")).toBe("true");
    });

    it("单击后左边树底部的属性面板跟着显示这台；再点同一台不收起，换一台跟着换", () => {
      render(<VaultView {...props()} />);
      const info = () => document.querySelector('[data-section="host-info"]');
      expect(info()).toBeNull();
      fireEvent.click(item(1));
      expect(info()).not.toBeNull();
      const first = info()?.textContent ?? "";
      fireEvent.click(item(1));
      expect(info()?.textContent).toBe(first);
      fireEvent.click(item(2));
      expect(info()?.textContent).not.toBe(first);
    });

    it("和左边树共用选中：树里单击一台，右边对应的卡片 / 行也高亮；反过来树里的行也高亮", () => {
      useSettings.setState({ "layout.vault_expanded": UNGROUPED_KEY });
      const handlers = props();
      render(<VaultView {...handlers} />);
      const treeRow = (name: string) =>
        [...document.querySelectorAll('[data-section="tree-host"]')].find((el) => el.textContent === name) as HTMLElement;
      fireEvent.click(treeRow("beta"));
      expect(item(2).getAttribute("data-selected")).toBe("true");
      expect(item(1).getAttribute("data-selected")).toBeNull();
      fireEvent.click(item(1));
      expect(item(1).getAttribute("data-selected")).toBe("true");
      expect(treeRow("alpha").className).toContain("bg-accent ");
      expect(treeRow("beta").className).not.toContain("bg-accent ");
      expect(handlers.onSelectHost).not.toHaveBeenCalled();
    });

    it("双击连接，不打开详情", () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      fireEvent.click(item(1));
      fireEvent.click(item(1));
      fireEvent.doubleClick(item(1));
      expect(handlers.onOpenHost).toHaveBeenCalledTimes(1);
      expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
      expect(handlers.onSelectHost).not.toHaveBeenCalled();
    });

    it("右键「编辑」打开详情（并选中）", async () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      await act(async () => fireEvent.contextMenu(item(2)));
      await act(async () => screen.getByText("编辑").click());
      expect(handlers.onSelectHost).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }));
      expect(item(2).getAttribute("data-selected")).toBe("true");
    });

    it("编辑按钮打开详情；它的点击和双击都不冒泡成选中 / 连接", () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      const button = item(1).querySelector('[data-action="edit-host"]') as HTMLElement;
      expect(button.getAttribute("title")).toBe("编辑");
      fireEvent.click(button);
      expect(handlers.onSelectHost).toHaveBeenCalledTimes(1);
      expect(handlers.onSelectHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
      expect(handlers.onOpenHost).not.toHaveBeenCalled();
      fireEvent.doubleClick(button);
      expect(handlers.onOpenHost).not.toHaveBeenCalled();
    });

    it("焦点在卡片 / 行上：Enter 连接，F2 打开详情；按钮上的按键不算", () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      fireEvent.keyDown(item(1), { key: "Enter" });
      expect(handlers.onOpenHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
      fireEvent.keyDown(item(1), { key: "F2" });
      expect(handlers.onSelectHost).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
      handlers.onOpenHost.mockClear();
      fireEvent.keyDown(item(1).querySelector('[data-action="edit-host"]') as HTMLElement, { key: "Enter" });
      expect(handlers.onOpenHost).not.toHaveBeenCalled();
    });
  });
}
