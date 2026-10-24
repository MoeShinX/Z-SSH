/** @vitest-environment jsdom */
//! 设置页「外观」里的「主机树」开关（UI-16）：读写 vault_ungrouped_flat（和「重命名「未分组」」同一项）。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "", known_hosts: "" })),
  openDataDir: vi.fn(async () => {}),
  sessionLogDirInfo: vi.fn(async () => ""),
  openSessionLogDir: vi.fn(async () => {}),
}));

import { settingsSet } from "../../api/settings";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import SettingsPage from "./SettingsPage";

afterEach(cleanup);

beforeEach(() => {
  vi.mocked(settingsSet).mockClear();
  useVault.setState({ proxies: [], loaded: true });
  useSettings.setState({ vault_ungrouped_flat: false } as never);
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("设置页：主机树开关", () => {
  it("外观页有开关和说明文字；打开 / 关上直接读写 vault_ungrouped_flat", async () => {
    render(<SettingsPage onClose={() => {}} />);
    expect(screen.getByText("主机树里不显示「未分组」文件夹，不分组的主机直接放在最下面")).not.toBeNull();
    expect(screen.getByText("打开后，没有分组的主机排在所有分组下面，中间用一条线隔开。")).not.toBeNull();
    const toggle = screen.getByTestId("ungrouped-flat").closest('[role="switch"]') as HTMLElement;
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await act(async () => fireEvent.click(toggle));
    expect(useSettings.getState().vault_ungrouped_flat).toBe(true);
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "true");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    await act(async () => fireEvent.click(toggle));
    expect(useSettings.getState().vault_ungrouped_flat).toBe(false);
    expect(settingsSet).toHaveBeenLastCalledWith("vault_ungrouped_flat", "false");
  });

  it("新用户（同步键没存过、本机默认是平铺）：开关显示开着；点关掉真的写下 false，之后一直是关", async () => {
    useSettings.setState({
      ungroupedFlatStored: false,
      vault_ungrouped_flat: false,
      "layout.vault_flat_default": "flat",
    } as never);
    render(<SettingsPage onClose={() => {}} />);
    const toggle = screen.getByTestId("ungrouped-flat").closest('[role="switch"]') as HTMLElement;
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(settingsSet).not.toHaveBeenCalled();
    await act(async () => fireEvent.click(toggle));
    expect(settingsSet).toHaveBeenCalledTimes(1);
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "false");
    expect(useSettings.getState().ungroupedFlatStored).toBe(true);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    // 本机默认没被改写
    expect(useSettings.getState()["layout.vault_flat_default"]).toBe("flat");
  });

  it("老用户（同步键没存过、本机默认是 classic）：开关关着；打开写 true", async () => {
    useSettings.setState({
      ungroupedFlatStored: false,
      vault_ungrouped_flat: false,
      "layout.vault_flat_default": "classic",
    } as never);
    render(<SettingsPage onClose={() => {}} />);
    const toggle = screen.getByTestId("ungrouped-flat").closest('[role="switch"]') as HTMLElement;
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await act(async () => fireEvent.click(toggle));
    expect(settingsSet).toHaveBeenCalledWith("vault_ungrouped_flat", "true");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
  });

  it("设置页打开时已经是平铺（比如刚重命名过「未分组」）：开关是开着的", () => {
    useSettings.setState({ vault_ungrouped_flat: true } as never);
    render(<SettingsPage onClose={() => {}} />);
    expect(screen.getByTestId("ungrouped-flat").closest('[role="switch"]')?.getAttribute("aria-checked")).toBe("true");
  });
});
