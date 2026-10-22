//! 三块同屏的宽度分配（M3-3 第 6.1 节）：各种窗口宽度，含必须收起 AI 的情况。

import { describe, expect, it } from "vitest";

import { compactColumns, computeDockLayout, DOCK_ORDER } from "./dockLayout";
import {
  clampPanelWidth,
  hostTreeRightReserved,
  PANEL_MIN,
  panelBounds,
  SFTP_PANEL_DEFAULT,
  TERMINAL_MIN,
} from "./panelWidths";

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

  it("复审给的那组数：窗口 1000、主机树 400、AI + SFTP——收起 AI，总宽不超窗口，终端不低于 360", () => {
    const layout = computeDockLayout({
      ...base,
      windowWidth: 1000,
      aiOpen: true,
      sftpOpen: true,
      reserved: 400,
    });
    expect(layout.collapseAi).toBe(true);
    expect(layout.aiWidth).toBe(0);
    // 扣掉主机树只剩 600：SFTP 最多 600 - 360 = 240
    expect(layout.sftpWidth).toBe(240);
    expect(layout.terminalWidth).toBe(TERMINAL_MIN);
    expect(400 + layout.aiWidth + layout.sftpWidth + layout.terminalWidth).toBe(1000);
  });

  it("主机树太宽导致两个面板收到最小也放不下：照样收起 AI（SFTP 保留），不再让终端变窄", () => {
    const layout = computeDockLayout({
      ...base,
      windowWidth: 1200,
      aiOpen: true,
      sftpOpen: true,
      reserved: 480,
    });
    expect(layout.collapseAi).toBe(true);
    expect(layout.terminalWidth).toBeGreaterThanOrEqual(TERMINAL_MIN);
    expect(480 + layout.sftpWidth + layout.terminalWidth).toBe(1200);
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

describe("主机树 + 终端 + 面板：总宽恒等于窗口宽，终端不低于 360（SFTP-9）", () => {
  const combos = [
    ["AI", true, false],
    ["SFTP", false, true],
    ["AI+SFTP", true, true],
  ] as const;

  it("主机树按它自己的上限（会让位给右边面板）取宽：700~2400、主机树 0/180/224/400/480、三种组合", () => {
    for (let width = 700; width <= 2400; width += 25) {
      for (const stored of [0, 180, 224, 400, 480]) {
        for (const [label, aiOpen, sftpOpen] of combos) {
          // 和 App 的接线一样：0 表示收起；展开时宽度被含右边面板占用的上限夹住
          const hostTree =
            stored === 0
              ? 0
              : clampPanelWidth("hostTree", stored, width, hostTreeRightReserved(aiOpen, sftpOpen));
          const layout = computeDockLayout({
            windowWidth: width,
            aiOpen,
            sftpOpen,
            aiDesired: 400,
            sftpDesired: 420,
            reserved: hostTree,
          });
          const where = `窗口 ${width} 主机树 ${stored}→${hostTree} ${label}`;
          expect(hostTree + layout.aiWidth + layout.sftpWidth + layout.terminalWidth, where).toBe(
            width,
          );
          expect(layout.terminalWidth, where).toBeGreaterThanOrEqual(TERMINAL_MIN);
          if (layout.collapseAi) {
            expect(layout.aiWidth, where).toBe(0);
            // 只有窗口本身太窄（主机树已是 180 仍放不下）才收起 AI
            expect(hostTree, where).toBeLessThanOrEqual(180);
          }
        }
      }
    }
  });

  it("不经上限夹取、硬塞一个很宽的主机树时也不超支（窗口够宽时终端 ≥ 360）", () => {
    for (let width = 900; width <= 2400; width += 25) {
      for (const reserved of [0, 180, 224, 400, 480]) {
        for (const [label, aiOpen, sftpOpen] of combos) {
          const layout = computeDockLayout({
            windowWidth: width,
            aiOpen,
            sftpOpen,
            aiDesired: 400,
            sftpDesired: 420,
            reserved,
          });
          const where = `窗口 ${width} 主机树 ${reserved} ${label}`;
          expect(reserved + layout.aiWidth + layout.sftpWidth + layout.terminalWidth, where).toBe(
            width,
          );
          expect(layout.terminalWidth, where).toBeGreaterThanOrEqual(TERMINAL_MIN);
        }
      }
    }
  });

  it("没有特殊照顾：放得下就都放，放不下就收 AI", () => {
    const fits = computeDockLayout({
      ...base,
      windowWidth: 1500,
      aiOpen: true,
      sftpOpen: true,
      reserved: 180,
    });
    expect(fits.collapseAi).toBe(false);
    // 窗口 1100、主机树 180：内容 920，360 + 640 = 1000 放不下
    const tight = computeDockLayout({
      ...base,
      windowWidth: 1100,
      aiOpen: true,
      sftpOpen: true,
      reserved: 180,
    });
    expect(tight.collapseAi).toBe(true);
  });
});

describe("主机树的上限随右边面板变化（SFTP-9）", () => {
  it("上限 = min(480, 窗口 40%, 窗口 - 360 - 右边面板最小宽度)，下限保底 180", () => {
    expect(hostTreeRightReserved(false, false)).toBe(0);
    expect(hostTreeRightReserved(true, false)).toBe(PANEL_MIN);
    expect(hostTreeRightReserved(false, true)).toBe(PANEL_MIN);
    expect(hostTreeRightReserved(true, true)).toBe(PANEL_MIN * 2);

    expect(panelBounds("hostTree", 1400, 0).max).toBe(480);
    expect(panelBounds("hostTree", 1400, 320).max).toBe(480); // 1400-360-320=720 > 480
    expect(panelBounds("hostTree", 1400, 640).max).toBe(400); // 1400-360-640
    expect(panelBounds("hostTree", 1000, 0).max).toBe(400);
    expect(panelBounds("hostTree", 1000, 320).max).toBe(320);
    expect(panelBounds("hostTree", 1000, 640)).toEqual({ min: 180, max: 180 }); // 挤到负数也保底 180
    expect(clampPanelWidth("hostTree", 400, 1000, 640)).toBe(180);
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
