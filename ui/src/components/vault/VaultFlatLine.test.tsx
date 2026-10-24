/** @vitest-environment jsdom */
//! 「未分组」用一条线代替（UI-16）：新用户默认平铺的判断、平铺模式下根分组和顶层主机之间的分隔线。

import { act, cleanup, fireEvent, render } from "@testing-library/react";
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

import { settingsGet, settingsSet } from "../../api/settings";
import type { Group, Host } from "../../api/vault";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { defaultFlatFor, resetFlatDefaultForTests, settleFlatDefault } from "../../state/ungroupedFlat";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import VaultView from "./VaultView";

const group = (id: number, name: string): Group => ({ id, name, parent_id: null, sort: 0 });
const host = (id: number, name: string, group_id: number | null) =>
  ({
    id,
    group_id,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags: [],
    sort: 0,
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
  resetFlatDefaultForTests();
  vi.mocked(settingsSet).mockClear();
  vi.mocked(settingsGet).mockReset();
  vi.mocked(settingsGet).mockResolvedValue(null);
  useSettings.setState({
    "layout.vault_expanded": "",
    "layout.vault_sidebar_collapsed": false,
    loaded: true,
    ungroupedFlatStored: false,
    vault_ungrouped_flat: false,
  } as never);
  useVault.setState({ groups: [], hosts: [], loaded: true, load: async () => {} } as never);
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("新用户默认平铺的判断", () => {
  it("没存过 + 主机库是空的 → 打开；没存过 + 已经有主机 → 关；存过 / 读库出错 → 不动", () => {
    expect(defaultFlatFor(false, 0)).toBe(true);
    expect(defaultFlatFor(false, 3)).toBe(false);
    expect(defaultFlatFor(true, 0)).toBeNull();
    expect(defaultFlatFor(true, 3)).toBeNull();
    expect(defaultFlatFor(null, 0)).toBeNull();
    expect(defaultFlatFor(null, 3)).toBeNull();
  });

  it("新装（空库、没存过）：写入 true，界面立刻是平铺", async () => {
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "true");
    expect(useSettings.getState().vault_ungrouped_flat).toBe(true);
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
  });

  it("老用户（没存过但库里有主机）：写入 false 保持原样；以后清空主机库也不会变成新用户", async () => {
    useVault.setState({ hosts: [host(1, "a1", null)] } as never);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "false");
    expect(useSettings.getState().vault_ungrouped_flat).toBe(false);
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
  });

  it("已经存过值：一个字都不写（开着的不会被盖掉，关着的不会被改成开）", async () => {
    useSettings.setState({ ungroupedFlatStored: true, vault_ungrouped_flat: true } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();
    expect(useSettings.getState().vault_ungrouped_flat).toBe(true);

    resetFlatDefaultForTests();
    useSettings.setState({ vault_ungrouped_flat: false } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();
    expect(useSettings.getState().vault_ungrouped_flat).toBe(false);
  });

  it("读设置出错（不知道存没存过）：不决定", async () => {
    useSettings.setState({ ungroupedFlatStored: null } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();
  });

  it("设置或主机库还没读完：先不决定；读完后只判断一次", async () => {
    useVault.setState({ loaded: false } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();
    useVault.setState({ loaded: true } as never);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
  });

  it("设置加载时记下库里有没有存过这一项；读库出错记为未知", async () => {
    vi.mocked(settingsGet).mockImplementation(async (key: string) => (key === "vault_ungrouped_flat" ? "true" : null));
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
    expect(useSettings.getState().vault_ungrouped_flat).toBe(true);

    vi.mocked(settingsGet).mockResolvedValue(null);
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBe(false);

    vi.mocked(settingsGet).mockImplementation(async (key: string) => {
      if (key === "vault_ungrouped_flat") throw new Error("boom");
      return null;
    });
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBeNull();
  });
});

describe("平铺模式的分隔线", () => {
  const divider = () => document.querySelector('[data-section="tree-flat-divider"]');
  const seed = (groups: Group[], hosts: Host[], flat = true) => {
    useVault.setState({ groups, hosts } as never);
    useSettings.setState({ vault_ungrouped_flat: flat } as never);
  };

  it("根分组和顶层主机两边都有：画一条线，在最后一个分组（含展开的内容）和第一台顶层主机之间", () => {
    seed([group(1, "Alpha")], [host(1, "a1", 1), host(2, "u1", null)]);
    useSettings.setState({ "layout.vault_expanded": "g1" } as never);
    render(<VaultView {...props()} />);
    const line = divider() as HTMLElement;
    expect(line).not.toBeNull();
    expect(line.getAttribute("role")).toBe("separator");
    expect(line.className).toContain("border-border/60");
    const rows = [...document.querySelectorAll('[data-section="tree-host"], [data-section="tree-flat-divider"]')];
    expect(rows.map((el) => el.textContent || "|")).toEqual(["a1", "|", "u1"]);
  });

  it("只有分组没有顶层主机，或只有顶层主机没有分组：不画", () => {
    seed([group(1, "Alpha")], [host(1, "a1", 1)]);
    const view = render(<VaultView {...props()} />);
    expect(divider()).toBeNull();
    view.unmount();
    seed([], [host(2, "u1", null)]);
    render(<VaultView {...props()} />);
    expect(divider()).toBeNull();
  });

  it("不是平铺模式（有「未分组」文件夹）：不画", () => {
    seed([group(1, "Alpha")], [host(2, "u1", null)], false);
    render(<VaultView {...props()} />);
    expect(divider()).toBeNull();
  });

  it("树里搜索后只剩一边：线跟着消失；两边都剩着就还在", async () => {
    seed([group(1, "Alpha")], [host(1, "a1", 1), host(2, "u1", null)]);
    useSettings.setState({ "layout.vault_expanded": "g1" } as never);
    render(<VaultView {...props()} />);
    const box = document.querySelector('[data-section="tree-search"]') as HTMLElement;
    await act(async () => fireEvent.change(box, { target: { value: "u1" } }));
    expect(divider()).toBeNull();
    await act(async () => fireEvent.change(box, { target: { value: "example" } }));
    expect(divider()).not.toBeNull();
  });

  it("右边的卡片里不画线", () => {
    seed([group(1, "Alpha")], [host(1, "a1", 1), host(2, "u1", null)]);
    render(<VaultView {...props()} />);
    expect(document.querySelector('[data-view] [data-section="tree-flat-divider"]')).toBeNull();
  });
});
