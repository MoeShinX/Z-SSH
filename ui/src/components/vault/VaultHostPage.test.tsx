/** @vitest-environment jsdom */
//! 主机页右边的卡片 / 列表（UI-16）：紧凑 / 舒适两档、排序方式、同层拖动和右键上移下移。

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

import { settingsSet } from "../../api/settings";
import type { Group, Host } from "../../api/vault";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import VaultView from "./VaultView";

const host = (id: number, name: string, group_id: number | null, tags: string[] = [], sort = 0) =>
  ({
    id,
    group_id,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags,
    sort,
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

let reorderHosts: ReturnType<typeof vi.fn>;
let saveHost: ReturnType<typeof vi.fn>;

function seed(groups: Group[], hosts: Host[]) {
  reorderHosts = vi.fn(async () => {});
  saveHost = vi.fn(async () => ({}) as Host);
  useVault.setState({ groups, hosts, load: async () => {}, reorderHosts, saveHost } as never);
}

function settings(extra: Record<string, unknown> = {}) {
  useSettings.setState({
    "layout.vault_expanded": "",
    "layout.vault_sidebar_collapsed": false,
    "layout.vault_view_mode": "grid",
    "layout.vault_density": "compact",
    "layout.recent_hosts": "",
    vault_ungrouped_flat: false,
    ...extra,
  } as never);
}

afterEach(cleanup);

beforeEach(() => {
  resetHostInfoForTests();
  vi.mocked(settingsSet).mockClear();
  seed([], [host(1, "alpha", null), host(2, "beta", null, ["prod", "web"])]);
  settings();
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const card = (id: number) => document.querySelector(`[data-section="host-card"][data-host-id="${id}"]`) as HTMLElement;
const row = (id: number) => document.querySelector(`[data-section="host-row"][data-host-id="${id}"]`) as HTMLElement;
const tokens = (el: Element) => el.className.split(/\s+/).filter(Boolean).sort();

describe("紧凑 / 舒适", () => {
  it("没存过这项设置时默认紧凑", () => {
    expect(useSettings.getState()["layout.vault_density"]).toBe("compact");
    render(<VaultView {...props()} />);
    const grid = document.querySelector('[data-view="grid"]') as HTMLElement;
    expect(grid.getAttribute("data-density")).toBe("compact");
    expect(grid.className).toContain("minmax(210px,1fr)");
    expect(card(1).className).toContain("p-2.5");
    expect(card(1).querySelector("img")?.getAttribute("width")).toBe("28");
  });

  it("舒适：和原来的样子完全一致（类名一个不多一个不少）", () => {
    settings({ "layout.vault_density": "comfortable" });
    render(<VaultView {...props()} />);
    const grid = document.querySelector('[data-view="grid"]') as HTMLElement;
    expect(tokens(grid)).toEqual("grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3".split(" ").sort());
    expect(tokens(card(1))).toEqual(
      (
        "group flex cursor-pointer flex-col rounded-lg border bg-card p-4 shadow-sm outline-none transition-colors hover:border-ring/60 hover:bg-accent/30 focus-visible:ring-1 focus-visible:ring-ring/60 border-border/60 relative"
      )
        .split(" ")
        .sort(),
    );
    // （relative 只是给拖动插入线当定位参照，不影响外观）
    // 没有标签的卡片也照旧有单独的底行，按钮在底行
    const edit = card(1).querySelector('[data-action="edit-host"]') as HTMLElement;
    expect(edit.className).toContain("h-8 w-8");
    expect(edit.closest(".mt-auto")).not.toBeNull();
    expect(card(1).querySelector("img")?.getAttribute("width")).toBe("34");
  });

  it("紧凑：没有标签的卡片，按钮和名字在同一行（不占单独一行）", () => {
    render(<VaultView {...props()} />);
    const edit = card(1).querySelector('[data-action="edit-host"]') as HTMLElement;
    expect(card(1).querySelector(".mt-auto")).toBeNull();
    const nameRow = (Array.from(card(1).querySelectorAll("div")).find((el) => el.textContent === "alpha") as HTMLElement)
      .parentElement?.parentElement as HTMLElement;
    expect(nameRow.contains(edit)).toBe(true);
    expect(edit.className).toContain("h-6 w-6");
  });

  it("紧凑：有标签的卡片，按钮放在标签那一行右边", () => {
    render(<VaultView {...props()} />);
    const bottom = card(2).querySelector(".mt-auto") as HTMLElement;
    expect(bottom).not.toBeNull();
    expect(bottom.textContent).toContain("prod");
    expect(bottom.querySelector('[data-action="edit-host"]')).not.toBeNull();
  });

  it("列表：紧凑行 py-1、小号按钮；舒适保持 py-2.5", () => {
    settings({ "layout.vault_view_mode": "list" });
    const view = render(<VaultView {...props()} />);
    expect(row(1).className).toContain("py-1");
    expect(row(1).className).not.toContain("py-2.5");
    expect((row(1).querySelector('[data-action="edit-host"]') as HTMLElement).className).toContain("h-6 w-6");
    view.unmount();
    settings({ "layout.vault_view_mode": "list", "layout.vault_density": "comfortable" });
    render(<VaultView {...props()} />);
    expect(row(1).className).toContain("py-2.5");
    expect(row(1).className).not.toContain("py-1 ");
    expect((row(1).querySelector('[data-action="edit-host"]') as HTMLElement).className).toContain("h-8 w-8");
  });

  it("顶部的按钮切换紧凑 / 舒适并写进设置", async () => {
    render(<VaultView {...props()} />);
    const toggle = document.querySelector('[data-action="toggle-density"]') as HTMLElement;
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    await act(async () => fireEvent.click(toggle));
    expect(useSettings.getState()["layout.vault_density"]).toBe("comfortable");
    expect(settingsSet).toHaveBeenCalledWith("layout.vault_density", JSON.stringify("comfortable"));
    expect(card(1).className).toContain("p-4");
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    await act(async () => fireEvent.click(toggle));
    expect(useSettings.getState()["layout.vault_density"]).toBe("compact");
    expect(screen.getByTitle(/紧凑/)).toBeTruthy();
  });
});
