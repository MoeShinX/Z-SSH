/** @vitest-environment jsdom */
//! 主机右键菜单的「复制密码」「复制连接信息」（UI-12）：主机页的卡片 / 列表 / 树，
//! 以及终端界面的树（直接用 HostTreeAside + 调用方传入的操作）。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", async (importOriginal) => ({
  draftOfHost: (await importOriginal<typeof import("../../api/vault")>()).draftOfHost,
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  copyHostPassword: vi.fn(async () => {}),
  copyHostInfo: vi.fn(async () => ({ with_password: true })),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import * as api from "../../api/vault";
import type { Host, Identity } from "../../api/vault";
import { useSettings } from "../../stores/settings";
import { resetHostInfoForTests } from "../../state/hostInfoPanel";
import { useVault } from "../../stores/vault";
import { UNGROUPED_KEY } from "../../state/hostTree";
import VaultView, { HostTreeAside, type HostActions } from "./VaultView";

const host = (id: number, name: string, extra: Partial<Host>) =>
  ({
    id,
    group_id: null,
    name,
    host: `${name}.example.internal`,
    port: 22,
    username: "root",
    tags: [],
    ...extra,
  }) as unknown as Host;

const identities: Identity[] = [
  { id: 10, name: "密码甲", kind: "password", key_path: null, has_secret: true },
  { id: 11, name: "密码乙(空)", kind: "password", key_path: null, has_secret: false },
  { id: 12, name: "私钥", kind: "key_inline", key_path: null, has_secret: true },
];

// 1 能复制；2 密钥登录；3 没引用凭据；4 私钥类凭据配密码登录；5 凭据没存密码
const hosts: Host[] = [
  host(1, "can-copy", { auth_method: "password", identity_id: 10 }),
  host(2, "by-key", { auth_method: "key", identity_id: 12 }),
  host(3, "no-identity", { auth_method: "password", identity_id: null }),
  host(4, "wrong-kind", { auth_method: "password", identity_id: 12 }),
  host(5, "empty-secret", { auth_method: "password", identity_id: 11 }),
];

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
  vi.mocked(api.copyHostPassword).mockReset().mockResolvedValue();
  vi.mocked(api.copyHostInfo).mockReset().mockResolvedValue({ with_password: true });
  useVault.setState({ groups: [], hosts, identities, load: async () => {} } as never);
  useSettings.setState({
    "layout.vault_expanded": UNGROUPED_KEY,
    "layout.vault_sidebar_collapsed": false,
  });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const openMenu = async (el: Element | null) => {
  expect(el).not.toBeNull();
  await act(async () => fireEvent.contextMenu(el as Element));
};
const hasItem = (label: string) => screen.queryByText(label) !== null;

/** 主机页的三个入口 + 终端界面的树，菜单应该一致。 */
const surfaces: { name: string; locate: (id: number) => Element | null; mode?: "grid" | "list" }[] = [
  {
    name: "主机页卡片",
    mode: "grid",
    locate: (id) => document.querySelector(`[data-section="host-card"][data-host-id="${id}"]`),
  },
  {
    name: "主机页列表行",
    mode: "list",
    locate: (id) => document.querySelector(`[data-section="host-row"][data-host-id="${id}"]`),
  },
  {
    name: "主机页左边的树",
    mode: "grid",
    locate: (id) => {
      const names = new Map(hosts.map((item) => [item.id, item.name]));
      return (
        [...document.querySelectorAll('[data-section="tree-host"]')].find(
          (el) => el.textContent === names.get(id),
        ) ?? null
      );
    },
  },
];

for (const { name, locate, mode } of surfaces) {
  describe(`${name}的右键菜单`, () => {
    beforeEach(() => {
      useSettings.setState({ "layout.vault_view_mode": mode ?? "grid" });
    });

    it("只在能复制时显示「复制密码」；「复制连接信息」所有主机都有", async () => {
      render(<VaultView {...props()} />);
      for (const item of hosts) {
        await openMenu(locate(item.id));
        expect(hasItem("复制密码"), `${item.name} 的「复制密码」`).toBe(item.id === 1);
        expect(hasItem("复制连接信息"), `${item.name} 的「复制连接信息」`).toBe(true);
        cleanup();
        render(<VaultView {...props()} />);
      }
    });

    it("点「复制密码」调用命令（只带主机 id）并提示 30 秒后清除", async () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      await openMenu(locate(1));
      await act(async () => screen.getByText("复制密码").click());
      expect(api.copyHostPassword).toHaveBeenCalledWith(1);
      await waitFor(() => expect(handlers.onNotice).toHaveBeenCalledWith("已复制密码，30 秒后自动清除"));
      expect(api.copyHostInfo).not.toHaveBeenCalled();
    });

    it("点「复制连接信息」调用命令，带密码和不带密码的提示不同", async () => {
      const handlers = props();
      render(<VaultView {...handlers} />);
      await openMenu(locate(1));
      await act(async () => screen.getByText("复制连接信息").click());
      expect(api.copyHostInfo).toHaveBeenCalledWith(1);
      await waitFor(() =>
        expect(handlers.onNotice).toHaveBeenCalledWith("已复制连接信息（含密码），30 秒后自动清除"),
      );

      vi.mocked(api.copyHostInfo).mockResolvedValue({ with_password: false });
      await openMenu(locate(2));
      await act(async () => screen.getByText("复制连接信息").click());
      expect(api.copyHostInfo).toHaveBeenLastCalledWith(2);
      await waitFor(() => expect(handlers.onNotice).toHaveBeenLastCalledWith("已复制连接信息"));
    });

    it("命令失败时提示失败原因", async () => {
      vi.mocked(api.copyHostPassword).mockRejectedValue("剪贴板被占用，请稍后再试");
      const handlers = props();
      render(<VaultView {...handlers} />);
      await openMenu(locate(1));
      await act(async () => screen.getByText("复制密码").click());
      await waitFor(() =>
        expect(handlers.onNotice).toHaveBeenCalledWith("复制密码失败：剪贴板被占用，请稍后再试"),
      );
    });
  });
}

describe("终端界面的主机树", () => {
  const actions = (): HostActions => ({
    select: vi.fn(),
    connect: vi.fn(),
    openSftp: vi.fn(),
    remove: vi.fn(),
    copy: vi.fn(),
    copyPassword: vi.fn(),
    copyInfo: vi.fn(),
  });
  const noop = () => {};
  const aside = (hostActions: HostActions) => (
    <HostTreeAside
      selectedGroup="all"
      onSelectGroup={noop}
      selectedHostId={null}
      hostActions={hostActions}
      infoPanel
      onNewHost={noop}
      onNewGroup={noop}
      onEditGroup={noop}
      onDeleteGroup={noop}
      collapsedStyle="hidden"
    />
  );
  const row = (name: string) =>
    [...document.querySelectorAll('[data-section="tree-host"]')].find((el) => el.textContent === name) ?? null;

  it("菜单项的显示条件和主机页一致，点了调用调用方传入的操作", async () => {
    const hostActions = actions();
    render(aside(hostActions));
    await openMenu(row("by-key"));
    expect(hasItem("复制密码")).toBe(false);
    expect(hasItem("复制连接信息")).toBe(true);
    await act(async () => screen.getByText("复制连接信息").click());
    expect(hostActions.copyInfo).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }));

    cleanup();
    render(aside(hostActions));
    await openMenu(row("can-copy"));
    await act(async () => screen.getByText("复制密码").click());
    expect(hostActions.copyPassword).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
  });
});
