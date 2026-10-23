/** @vitest-environment jsdom */
//! 主机详情里的分组下拉（UI-14）：「未分组」改过名之后第一项叫「（顶层）」；没改过名照旧是「未分组」。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  hostMemoryGet: vi.fn(async () => null),
  hostMemorySet: vi.fn(async () => {}),
  saveProxy: vi.fn(),
  saveHost: vi.fn(),
  saveIdentity: vi.fn(),
  saveGroup: vi.fn(),
  deleteGroup: vi.fn(),
  deleteHost: vi.fn(),
  deleteIdentity: vi.fn(),
  deleteProxy: vi.fn(),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/auth", () => ({
  hostkeySaved: vi.fn(async () => null),
  hostkeyForget: vi.fn(async () => 0),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import { deleteGroupMessage, ungroupedLabel, ungroupedMoveLabel, ungroupedOptionLabel } from "../../state/ungroupedFlat";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import HostDetailsPanel from "./HostDetailsPanel";

afterEach(cleanup);

beforeEach(() => {
  useVault.setState({
    groups: [{ id: 1, name: "Alpha", parent_id: null, sort: 0 }],
    hosts: [],
    identities: [],
    proxies: [],
    loaded: true,
  });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  // Radix Select 在 jsdom 里要这几个
  Element.prototype.hasPointerCapture = Element.prototype.hasPointerCapture ?? (() => false);
  Element.prototype.scrollIntoView = Element.prototype.scrollIntoView ?? (() => {});
});

function groupTrigger(): HTMLElement {
  const label = screen.getAllByText("分组").find((el) => el.tagName === "LABEL");
  const trigger = label?.parentElement?.querySelector('[role="combobox"]');
  if (!trigger) throw new Error("找不到分组下拉");
  return trigger as HTMLElement;
}

function mount() {
  render(
    <HostDetailsPanel
      host={null}
      groupId={null}
      onClose={() => {}}
      onSaved={() => {}}
      onDelete={() => {}}
      onError={() => {}}
    />,
  );
}

describe("新建主机的分组下拉", () => {
  it("平铺模式：默认选中「（顶层）」，下拉里没有「未分组」", () => {
    useSettings.setState({ vault_ungrouped_flat: true });
    mount();
    expect(groupTrigger().textContent).toBe("（顶层）");
    fireEvent.keyDown(groupTrigger(), { key: "ArrowDown" });
    const options = screen.getAllByRole("option").map((el) => el.textContent);
    expect(options).toEqual(["（顶层）", "Alpha"]);
    expect(screen.queryByText("未分组")).toBeNull();
  });

  it("没改过名：还是「未分组」", () => {
    useSettings.setState({ vault_ungrouped_flat: false });
    mount();
    expect(groupTrigger().textContent).toBe("未分组");
    fireEvent.keyDown(groupTrigger(), { key: "ArrowDown" });
    expect(screen.getAllByRole("option").map((el) => el.textContent)).toEqual(["未分组", "Alpha"]);
  });
});

describe("文案", () => {
  it("开关关着是「未分组」，打开后是「顶层」", () => {
    expect(ungroupedLabel(false)).toBe("未分组");
    expect(ungroupedLabel(true)).toBe("顶层");
    expect(ungroupedOptionLabel(false)).toBe("未分组");
    expect(ungroupedOptionLabel(true)).toBe("（顶层）");
    expect(ungroupedMoveLabel(false)).toBe("未分组");
    expect(ungroupedMoveLabel(true)).toBe("顶层（不分组）");
  });

  it("删除分组的确认文案", () => {
    expect(deleteGroupMessage(false)).toBe("分组下的主机会变成未分组，不会被删。");
    expect(deleteGroupMessage(true)).toBe("分组下的主机会移到顶层，不会被删。");
    expect(deleteGroupMessage(true)).not.toContain("未分组");
  });
});
