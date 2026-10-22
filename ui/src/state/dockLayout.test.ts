//! 三块同屏的宽度分配（M3-3 第 6.1 节）：各种窗口宽度，含必须收起 AI 的情况。

import { describe, expect, it } from "vitest";

import { compactColumns, computeDockLayout, DOCK_ORDER } from "./dockLayout";
import { clampPanelWidth, panelBounds, SFTP_PANEL_DEFAULT, TERMINAL_MIN } from "./panelWidths";

const base = { aiDesired: 400, sftpDesired: 420 };

describe("computeDockLayout", () => {
  it("都没开：终端占满", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1400, aiOpen: false, sftpOpen: false });
    expect(layout).toEqual({ aiWidth: 0, sftpWidth: 0, collapseAi: false, terminalWidth: 1400 });
  });

  it("只开 SFTP：紧挨终端，默认 420", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1400, aiOpen: false, sftpOpen: true });
    expect(layout.sftpWidth).toBe(420);
    expect(layout.aiWidth).toBe(0);
    expect(layout.terminalWidth).toBe(980);
  });

  it("只开 AI：保持 UI-1 的行为", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1400, aiOpen: true, sftpOpen: false });
    expect(layout.aiWidth).toBe(400);
  });

  it("1400 宽两个都开：各自默认宽度，终端 580", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1400, aiOpen: true, sftpOpen: true });
    expect(layout).toEqual({ aiWidth: 400, sftpWidth: 420, collapseAi: false, terminalWidth: 580 });
  });

  it("1100 宽：放不下默认值，两个面板往最小宽度收，终端正好 360", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1100, aiOpen: true, sftpOpen: true });
    expect(layout.collapseAi).toBe(false);
    expect(layout.terminalWidth).toBe(TERMINAL_MIN);
    expect(layout.aiWidth + layout.sftpWidth).toBe(1100 - TERMINAL_MIN);
    expect(layout.aiWidth).toBeGreaterThanOrEqual(320);
    expect(layout.sftpWidth).toBeGreaterThanOrEqual(320);
  });

  it("1000 宽（刚好 2×320+360）：两块都是最小宽度，不收起", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1000, aiOpen: true, sftpOpen: true });
    expect(layout).toEqual({ aiWidth: 320, sftpWidth: 320, collapseAi: false, terminalWidth: 360 });
  });

  it("999 宽：收到最小也放不下，收起 AI，SFTP 保留", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 999, aiOpen: true, sftpOpen: true });
    expect(layout.collapseAi).toBe(true);
    expect(layout.aiWidth).toBe(0);
    expect(layout.sftpWidth).toBe(420);
    expect(layout.terminalWidth).toBe(999 - 420);
  });

  it("900 宽（窗口最小值）：收起 AI，终端仍不低于 360", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 900, aiOpen: true, sftpOpen: true });
    expect(layout.collapseAi).toBe(true);
    expect(layout.terminalWidth).toBeGreaterThanOrEqual(TERMINAL_MIN);
  });

  it("窗口很小：只开 SFTP 也不挤没终端", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 600, aiOpen: false, sftpOpen: true });
    expect(layout.sftpWidth).toBe(240);
    expect(layout.terminalWidth).toBe(360);
  });

  it("记住的宽度被夹到合法区间（太小抬到 320，太大压到 60%）", () => {
    const small = computeDockLayout({ windowWidth: 1400, aiOpen: false, sftpOpen: true, aiDesired: 400, sftpDesired: 100 });
    expect(small.sftpWidth).toBe(320);
    const big = computeDockLayout({ windowWidth: 1400, aiOpen: false, sftpOpen: true, aiDesired: 400, sftpDesired: 5000 });
    expect(big.sftpWidth).toBe(840);
  });

  it("左边展开的主机树先扣掉：面板往最小收，终端仍留 360", () => {
    const layout = computeDockLayout({
      ...base,
      windowWidth: 1400,
      aiOpen: true,
      sftpOpen: true,
      reserved: 300,
    });
    expect(layout.collapseAi).toBe(false);
    expect(layout.terminalWidth).toBe(TERMINAL_MIN);
    expect(layout.aiWidth + layout.sftpWidth + layout.terminalWidth).toBe(1400 - 300);
  });

  it("只因为主机树太宽才放不下：不收起 AI，两个面板最小宽度，终端让出来", () => {
    const layout = computeDockLayout({
      ...base,
      windowWidth: 1200,
      aiOpen: true,
      sftpOpen: true,
      reserved: 480,
    });
    expect(layout).toEqual({ aiWidth: 320, sftpWidth: 320, collapseAi: false, terminalWidth: 80 });
  });

  it("只开一个面板时主机树占的宽度同样扣掉", () => {
    const layout = computeDockLayout({ ...base, windowWidth: 1000, aiOpen: false, sftpOpen: true, reserved: 224 });
    expect(layout.sftpWidth).toBe(1000 - 224 - TERMINAL_MIN);
    expect(layout.terminalWidth).toBe(TERMINAL_MIN);
  });

  it("任何窗口宽度下终端都不低于 360，三块加起来正好是窗口宽度", () => {
    for (let width = 700; width <= 2400; width += 37) {
      for (const [aiOpen, sftpOpen] of [[true, true], [true, false], [false, true]] as const) {
        const layout = computeDockLayout({ ...base, windowWidth: width, aiOpen, sftpOpen });
        expect(layout.terminalWidth, `窗口 ${width}`).toBeGreaterThanOrEqual(TERMINAL_MIN);
        expect(layout.aiWidth + layout.sftpWidth + layout.terminalWidth).toBe(width);
      }
    }
  });
});

describe("SFTP 面板的拖宽上下限", () => {
  it("默认 420；两块同开时对方占的宽度从可用空间里扣掉", () => {
    expect(SFTP_PANEL_DEFAULT).toBe(420);
    expect(panelBounds("sftp", 1400)).toEqual({ min: 320, max: 840 });
    expect(panelBounds("sftp", 1400, 400)).toEqual({ min: 320, max: 640 });
    expect(clampPanelWidth("sftp", 900, 1400, 400)).toBe(640);
  });
});

describe("面板顺序固定", () => {
  it("终端｜AI｜SFTP，不能调换", () => {
    expect([...DOCK_ORDER]).toEqual(["terminal", "ai", "sftp"]);
  });
});

describe("窄面板的列精简", () => {
  it("宽度够就是 名称、大小、修改时间；不够先藏修改时间", () => {
    expect(compactColumns(420)).toEqual(["name", "size", "mtime"]);
    expect(compactColumns(300)).toEqual(["name", "size"]);
  });
});
