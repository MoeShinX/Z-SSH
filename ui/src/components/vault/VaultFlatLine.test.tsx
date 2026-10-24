/** @vitest-environment jsdom */
//! 「未分组」用一条线代替（UI-16）和新用户默认平铺只存本机（UI-18）：有效值、首次判断、
//! 明确选择写同步键，以及平铺模式下根分组和顶层主机之间的分隔线。

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

import { settingsGet, settingsSet } from "../../api/settings";
import type { Group, Host } from "../../api/vault";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import {
  defaultFlatFor,
  effectiveFlat,
  effectiveUngroupedFlat,
  isUngroupedFlat,
  resetFlatDefaultForTests,
  settleFlatDefault,
} from "../../state/ungroupedFlat";
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
  onOpenProxies: vi.fn(),
  onOpenForwards: vi.fn(),
  onNotice: vi.fn(),
});

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  resetFlatDefaultForTests();
  vi.mocked(settingsSet).mockReset();
  vi.mocked(settingsSet).mockResolvedValue(undefined);
  vi.mocked(settingsGet).mockReset();
  vi.mocked(settingsGet).mockResolvedValue(null);
  useSettings.setState({
    "layout.vault_expanded": "",
    "layout.vault_sidebar_collapsed": false,
    "layout.vault_flat_default": "",
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

describe("有效值：现在是不是平铺", () => {
  it("同步键存过值 → 用它（不管本机默认）", () => {
    expect(effectiveFlat(true, true, "")).toBe(true);
    expect(effectiveFlat(true, true, "classic")).toBe(true);
    expect(effectiveFlat(true, false, "flat")).toBe(false);
    expect(effectiveFlat(true, false, "")).toBe(false);
  });

  it("同步键没存过 → 看本机默认是不是 flat", () => {
    expect(effectiveFlat(false, false, "flat")).toBe(true);
    expect(effectiveFlat(false, false, "classic")).toBe(false);
    expect(effectiveFlat(false, false, "")).toBe(false);
    expect(effectiveFlat(false, false, "乱写的值")).toBe(false);
  });

  it("不知道存没存过（读库出错）→ 用同步键的值（没读到就是关）", () => {
    expect(effectiveFlat(null, false, "flat")).toBe(false);
    expect(effectiveFlat(null, true, "")).toBe(true);
  });

  it("从设置里取：isUngroupedFlat 和 effectiveUngroupedFlat 走同一个算法", () => {
    useSettings.setState({
      ungroupedFlatStored: false,
      vault_ungrouped_flat: false,
      "layout.vault_flat_default": "flat",
    } as never);
    expect(isUngroupedFlat()).toBe(true);
    expect(effectiveUngroupedFlat(useSettings.getState())).toBe(true);
    useSettings.setState({ ungroupedFlatStored: true } as never);
    expect(isUngroupedFlat()).toBe(false);
  });
});

describe("新用户默认平铺的首次判断（只存本机）", () => {
  it("判断规则：已判断过不动；存过同步值 → classic；没存过：空库 flat、有主机 classic；读库出错不决定", () => {
    expect(defaultFlatFor("", false, 0)).toBe("flat");
    expect(defaultFlatFor("", false, 3)).toBe("classic");
    expect(defaultFlatFor("", true, 0)).toBe("classic");
    expect(defaultFlatFor("", true, 3)).toBe("classic");
    expect(defaultFlatFor("flat", false, 3)).toBeNull();
    expect(defaultFlatFor("classic", false, 0)).toBeNull();
    expect(defaultFlatFor("", null, 0)).toBeNull();
    expect(defaultFlatFor("", null, 3)).toBeNull();
    // 本机的值被改坏了（不是 flat / classic）：当成没判断过，重新判断
    expect(defaultFlatFor("乱写的值", false, 0)).toBe("flat");
  });

  it("新装（空库、没存过）：本机默认写 flat，界面平铺；库里没有出现 vault_ungrouped_flat", async () => {
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_flat_default", '"flat"');
    expect(vi.mocked(settingsSet).mock.calls.some(([key]) => key === "vault_ungrouped_flat")).toBe(false);
    expect(useSettings.getState()["layout.vault_flat_default"]).toBe("flat");
    // 同步键没被动：没存过、仍是默认值
    expect(useSettings.getState().ungroupedFlatStored).toBe(false);
    expect(useSettings.getState().vault_ungrouped_flat).toBe(false);
    expect(isUngroupedFlat()).toBe(true);
  });

  it("新装后树真的是平铺：没有「未分组」行，根分组和顶层主机之间有线", async () => {
    // 首次启动时库是空的；之后用户才添加分组和主机
    await settleFlatDefault();
    useVault.setState({ groups: [group(1, "Alpha")], hosts: [host(1, "a1", 1), host(2, "u1", null)] } as never);
    useSettings.setState({ "layout.vault_expanded": "g1" } as never);
    render(<VaultView {...props()} />);
    expect(screen.queryByText("未分组")).toBeNull();
    expect(document.querySelector('[data-section="tree-flat-divider"]')).not.toBeNull();
  });

  it("老用户（没存过但库里有主机）：本机默认写 classic，树和以前一样；同步设置里一个字都不写", async () => {
    useVault.setState({ groups: [group(1, "Alpha")], hosts: [host(1, "a1", null)] } as never);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_flat_default", '"classic"');
    expect(isUngroupedFlat()).toBe(false);
    render(<VaultView {...props()} />);
    expect(screen.getAllByText("未分组").length).toBeGreaterThan(0);
  });

  it("同步键已经存过值（开着）：本机默认写 classic、不起作用，界面仍按同步键", async () => {
    useSettings.setState({ ungroupedFlatStored: true, vault_ungrouped_flat: true } as never);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledTimes(1);
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_flat_default", '"classic"');
    expect(isUngroupedFlat()).toBe(true);
  });

  it("同步键已经存过值（关着）：空库也不会被判成平铺", async () => {
    useSettings.setState({ ungroupedFlatStored: true, vault_ungrouped_flat: false } as never);
    await settleFlatDefault();
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_flat_default", '"classic"');
    expect(isUngroupedFlat()).toBe(false);
  });

  it("已经判断过（flat / classic）：一个字都不写", async () => {
    useSettings.setState({ "layout.vault_flat_default": "classic" } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();

    resetFlatDefaultForTests();
    useSettings.setState({ "layout.vault_flat_default": "flat" } as never);
    await settleFlatDefault();
    expect(settingsSet).not.toHaveBeenCalled();
    expect(isUngroupedFlat()).toBe(true);
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

  it("已经按 UI-16 旧逻辑跑过的本机（同步键被写成 true / false）：照旧用它，不迁移", () => {
    useSettings.setState({ ungroupedFlatStored: true, vault_ungrouped_flat: true } as never);
    expect(isUngroupedFlat()).toBe(true);
    useSettings.setState({ vault_ungrouped_flat: false } as never);
    expect(isUngroupedFlat()).toBe(false);
  });

  it("设置加载时记下库里有没有存过同步键、读回本机默认；读库出错记为未知", async () => {
    vi.mocked(settingsGet).mockImplementation(async (key: string) => {
      if (key === "vault_ungrouped_flat") return "true";
      if (key === "layout.vault_flat_default") return '"classic"';
      return null;
    });
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
    expect(useSettings.getState().vault_ungrouped_flat).toBe(true);
    expect(useSettings.getState()["layout.vault_flat_default"]).toBe("classic");

    vi.mocked(settingsGet).mockResolvedValue(null);
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBe(false);
    expect(useSettings.getState()["layout.vault_flat_default"]).toBe("");

    vi.mocked(settingsGet).mockImplementation(async (key: string) => {
      if (key === "vault_ungrouped_flat") throw new Error("boom");
      return null;
    });
    await useSettings.getState().load();
    expect(useSettings.getState().ungroupedFlatStored).toBeNull();
  });
});

describe("明确选择写同步键（setUngroupedFlat）", () => {
  it("没存过时打开：写 true，马上改用同步键", async () => {
    expect(await useSettings.getState().setUngroupedFlat(true)).toBe(true);
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "true");
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
    expect(isUngroupedFlat()).toBe(true);
  });

  it("本机默认是平铺、同步键没存过时关掉：真的写下 false，生效值变成不平铺", async () => {
    useSettings.setState({ "layout.vault_flat_default": "flat" } as never);
    expect(isUngroupedFlat()).toBe(true);
    expect(await useSettings.getState().setUngroupedFlat(false)).toBe(true);
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "false");
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
    expect(isUngroupedFlat()).toBe(false);
  });

  it("写库失败：回滚到原来的状态（还是没存过，生效值不变）", async () => {
    useSettings.setState({ "layout.vault_flat_default": "flat" } as never);
    vi.mocked(settingsSet).mockRejectedValueOnce(new Error("boom"));
    expect(await useSettings.getState().setUngroupedFlat(false)).toBe(false);
    expect(useSettings.getState().ungroupedFlatStored).toBe(false);
    expect(useSettings.getState().vault_ungrouped_flat).toBe(false);
    expect(isUngroupedFlat()).toBe(true);
  });
});

describe("平铺模式的分隔线", () => {
  const divider = () => document.querySelector('[data-section="tree-flat-divider"]');
  // 固定成「同步键存过值」，免得受本机默认影响
  const seed = (groups: Group[], hosts: Host[], flat = true) => {
    useVault.setState({ groups, hosts } as never);
    useSettings.setState({ ungroupedFlatStored: true, vault_ungrouped_flat: flat } as never);
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
