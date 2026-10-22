//! 终端｜AI｜SFTP 三块同屏的宽度分配（M3-3 第 1 节）：纯函数，方便单测。
//!
//! 规则：
//! - 顺序固定（终端、AI、SFTP），这里只算宽度，不排顺序；
//! - 终端至少保留 360px（左边展开的主机树占的宽度先扣掉，见 `reserved`）；
//! - 放不下时先把两个面板往各自的最小宽度（320px）收，按"各自还能收多少"的比例分摊；
//! - 收到最小还放不下，就收起 AI 面板（SFTP 是用户刚打开的，优先保留）。
//!   窗口变宽后不自动再展开，由调用方决定。

import {
  AI_PANEL_MAX_RATIO,
  PANEL_MIN,
  SFTP_PANEL_MAX_RATIO,
  TERMINAL_MIN,
} from "./panelWidths";

export interface DockLayoutInput {
  windowWidth: number;
  aiOpen: boolean;
  sftpOpen: boolean;
  /** 用户记住的宽度（设置表里的值，已经体检过）。 */
  aiDesired: number;
  sftpDesired: number;
  /** 左边已经占掉的宽度（终端界面里展开的主机树），不能分给终端和面板。 */
  reserved?: number;
}

export interface DockLayout {
  /** 实际宽度；没开或者被收起的是 0。 */
  aiWidth: number;
  sftpWidth: number;
  /** 两个都开、窗口又太窄：AI 面板必须收起。调用方负责真的关掉并提示。 */
  collapseAi: boolean;
  /** 终端剩下的宽度。 */
  terminalWidth: number;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round(value)));
}

export function computeDockLayout(input: DockLayoutInput): DockLayout {
  const { windowWidth, aiOpen, sftpOpen, aiDesired, sftpDesired } = input;
  // 面板的比例上限仍按窗口宽算（主机树开关不该让面板忽宽忽窄），终端保底按扣掉主机树之后算
  const content = Math.max(0, windowWidth - Math.max(0, input.reserved ?? 0));
  const available = Math.max(0, Math.floor(content - TERMINAL_MIN));
  const single = (desired: number, ratio: number): number => {
    const max = Math.max(0, Math.floor(Math.min(windowWidth * ratio, available)));
    const min = Math.min(PANEL_MIN, max);
    return clamp(desired, min, max);
  };

  if (!aiOpen && !sftpOpen) {
    return { aiWidth: 0, sftpWidth: 0, collapseAi: false, terminalWidth: content };
  }
  if (aiOpen && !sftpOpen) {
    const aiWidth = single(aiDesired, AI_PANEL_MAX_RATIO);
    return { aiWidth, sftpWidth: 0, collapseAi: false, terminalWidth: content - aiWidth };
  }
  if (!aiOpen && sftpOpen) {
    const sftpWidth = single(sftpDesired, SFTP_PANEL_MAX_RATIO);
    return { aiWidth: 0, sftpWidth, collapseAi: false, terminalWidth: content - sftpWidth };
  }

  // 两个都开。
  let ai = clamp(aiDesired, PANEL_MIN, Math.max(PANEL_MIN, Math.floor(windowWidth * AI_PANEL_MAX_RATIO)));
  let sftp = clamp(
    sftpDesired,
    PANEL_MIN,
    Math.max(PANEL_MIN, Math.floor(windowWidth * SFTP_PANEL_MAX_RATIO)),
  );
  const excess = ai + sftp - available;
  if (excess <= 0) {
    return { aiWidth: ai, sftpWidth: sftp, collapseAi: false, terminalWidth: content - ai - sftp };
  }
  const slackAi = ai - PANEL_MIN;
  const slackSftp = sftp - PANEL_MIN;
  if (slackAi + slackSftp >= excess) {
    const cutAi = Math.min(slackAi, Math.round((excess * slackAi) / (slackAi + slackSftp)));
    const cutSftp = excess - cutAi;
    ai -= cutAi;
    sftp -= cutSftp;
    return { aiWidth: ai, sftpWidth: sftp, collapseAi: false, terminalWidth: content - ai - sftp };
  }
  // 两个都收到最小也放不下。只是因为主机树拉宽了才放不下时，不替用户关 AI 面板
  // （拖主机树把 AI 面板拖没了会很意外）：两个面板都按最小宽度，终端让出来。
  if (PANEL_MIN * 2 <= Math.floor(windowWidth - TERMINAL_MIN)) {
    return {
      aiWidth: PANEL_MIN,
      sftpWidth: PANEL_MIN,
      collapseAi: false,
      terminalWidth: Math.max(0, content - PANEL_MIN * 2),
    };
  }
  // 窗口本身就放不下：收起 AI，SFTP 单独排。
  const sftpAlone = single(sftpDesired, SFTP_PANEL_MAX_RATIO);
  return {
    aiWidth: 0,
    sftpWidth: sftpAlone,
    collapseAi: true,
    terminalWidth: content - sftpAlone,
  };
}

/** 面板顺序：从左到右固定，测试和渲染都认这一份。 */
export const DOCK_ORDER = ["terminal", "ai", "sftp"] as const;

/** 面板窄时的列精简（M3-3 第 3 节）：按面板像素宽度给出要显示哪些列。 */
export function compactColumns(paneWidth: number): Array<"name" | "size" | "mtime"> {
  // 名称至少要有 120px 的空间；大小 64px、修改时间 128px（含间隙）。
  if (paneWidth >= 120 + 64 + 128 + 40) return ["name", "size", "mtime"];
  return ["name", "size"];
}
