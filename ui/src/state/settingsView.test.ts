/** @vitest-environment jsdom */
import { describe, expect, it } from "vitest";

import {
  closeSettings,
  escClosesSettings,
  hasOpenLayer,
  openSettings,
  toggleSettings,
} from "./settingsView";

describe("设置页的进出", () => {
  it("从终端进设置，再点齿轮回终端", () => {
    const inSettings = toggleSettings({ view: "terminal", returnTo: "vault" }, true);
    expect(inSettings).toEqual({ view: "settings", returnTo: "terminal" });
    expect(toggleSettings(inSettings, true).view).toBe("terminal");
  });

  it("从主机页进设置，关掉回主机页", () => {
    const inSettings = openSettings({ view: "vault", returnTo: "vault" });
    expect(closeSettings(inSettings, true).view).toBe("vault");
  });

  it("已经在设置页再打开：不把 returnTo 改掉", () => {
    const nav = { view: "settings", returnTo: "terminal" } as const;
    expect(openSettings(nav)).toBe(nav);
  });

  it("进来时在终端、期间标签全关了：退回主机页", () => {
    expect(closeSettings({ view: "settings", returnTo: "terminal" }, false).view).toBe("vault");
  });

  it("不在设置页时关设置什么也不做", () => {
    const nav = { view: "terminal", returnTo: "vault" } as const;
    expect(closeSettings(nav, true)).toBe(nav);
  });
});

describe("Esc 关设置页", () => {
  const esc = { key: "Escape", defaultPrevented: false, isComposing: false };

  it("设置页、没有弹层：关", () => {
    expect(escClosesSettings(esc, "settings", false)).toBe(true);
  });

  it("不在设置页 / 不是 Esc：不关", () => {
    expect(escClosesSettings(esc, "terminal", false)).toBe(false);
    expect(escClosesSettings({ ...esc, key: "Enter" }, "settings", false)).toBe(false);
  });

  it("弹层已经处理了这次 Esc，或者弹层还开着：只关弹层", () => {
    expect(escClosesSettings({ ...esc, defaultPrevented: true }, "settings", false)).toBe(false);
    expect(escClosesSettings(esc, "settings", true)).toBe(false);
  });

  it("输入法组字时的 Esc 不算", () => {
    expect(escClosesSettings({ ...esc, isComposing: true }, "settings", false)).toBe(false);
  });

  it("hasOpenLayer 认得打开的下拉框和对话框，不认关着的", () => {
    document.body.innerHTML = '<div role="dialog" data-state="closed"></div>';
    expect(hasOpenLayer(document)).toBe(false);
    document.body.innerHTML = '<div role="listbox" data-state="open"></div>';
    expect(hasOpenLayer(document)).toBe(true);
    document.body.innerHTML = '<div role="dialog" data-state="open"></div>';
    expect(hasOpenLayer(document)).toBe(true);
    document.body.innerHTML = "";
  });
});
