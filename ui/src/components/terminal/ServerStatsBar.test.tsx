/** @vitest-environment jsdom */
//! 状态栏的纯函数 + 数据接线（M1-10；组件用例是 M1-10b 补的）。
//!
//! M1-10b 的回归背景：后端序列化字段名曾是 `mem_*_mb`（值其实是 MiB），
//! 前端按 `mem_*_mib` 读，内存显示成 `--/--`。这里用一份固定的
//! `ServerStats` 数据把状态栏和四个悬停详情渲染出的文字全部钉住
//! （内存明细为主，CPU / 磁盘 / 网速顺带覆盖同类字段）。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ServerStats } from "../../api/session";
import ServerStatsBar, {
  formatCpuHeading,
  formatMibAsG,
  formatNetSpeed,
  formatTraffic,
  formatUptime,
} from "./ServerStatsBar";

/** 固定的服务器状态（一台 2 核 / 4GiB / 40GiB 磁盘的机器）。 */
const FIXTURE: ServerStats = {
  cpu_percent: 42,
  per_core_percent: [90, 10],
  cpu_cores: 2,
  mem_total_mib: 4096,
  mem_used_mib: 2048,
  mem_free_mib: 1024,
  mem_buffers_mib: 200,
  mem_cached_mib: 512,
  swap_total_mib: 2048,
  swap_used_mib: 1024,
  disk_total_mib: 40960,
  disk_used_mib: 10240,
  disk_percent: 25,
  net_rx_speed: 2048,
  net_tx_speed: 1024 * 1024,
  net_interfaces: [{ name: "eth0", rx_speed: 2048, tx_speed: 1024 * 1024 }],
  top_processes: [{ pid: "1", mem_percent: 12.5, command: "nginx" }],
  hostname: "demo-host",
  os_name: "Debian GNU/Linux 12",
  kernel: "6.1.0-test",
  uptime_secs: 90061,
  load_1m: 0.5,
  load_5m: 0.25,
  load_15m: 0.1,
};

beforeEach(() => {
  // jsdom 没有 ResizeObserver；Popover 的浮层定位要它。
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterEach(cleanup);

function renderBar(stats: ServerStats | null = FIXTURE) {
  return render(<ServerStatsBar stats={stats} connected />);
}

/** Row 结构是 <span>标签</span><span>值</span>，取标签后面那个值。 */
function rowValue(label: string): string | null | undefined {
  return screen.getByText(label).nextElementSibling?.textContent;
}

describe("ServerStatsBar 数据接线（固定数据）", () => {
  it("状态栏：CPU / 内存 / 磁盘 / 网速都显示数值，不再有 --", async () => {
    renderBar();
    expect(screen.getByText("42%")).toBeTruthy();
    // M1-10b 的回归点：字段名接错时这里是 "--/--"。
    expect(screen.getByText("2.0GB/4.0GB")).toBeTruthy();
    expect(screen.getByText("10GB/40GB")).toBeTruthy();
    expect(screen.getByText("2.0KB/s")).toBeTruthy();
    expect(screen.getByText("1.0MB/s")).toBeTruthy();
    expect(screen.queryByText("--")).toBeNull();
  });

  it("点状态栏开关概览；点悬停详情不算", () => {
    const onToggle = vi.fn();
    render(<ServerStatsBar stats={FIXTURE} connected onToggleOverview={onToggle} />);
    fireEvent.click(document.querySelector('[data-section="stats-overview-toggle"]')!);
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("状态栏显示开机以来的时间；没有数据时不显示这一段", () => {
    const { unmount } = renderBar();
    expect(document.querySelector('[data-section="stats-uptime"]')?.textContent).toMatch(/^运行 /);
    unmount();
    renderBar({ ...FIXTURE, uptime_secs: null });
    expect(document.querySelector('[data-section="stats-uptime"]')).toBeNull();
  });

  it("内存详情：已用 / 可用 / buffers / cached / 交换分区 / 总量", async () => {
    renderBar();
    fireEvent.mouseOver(
      screen.getByTitle("内存（悬停看明细和占用最多的进程）"),
    );
    expect(await screen.findByText("内存")).toBeTruthy();
    expect(rowValue("已用")).toBe("2.0GB");
    expect(rowValue("可用")).toBe("1.0GB");
    expect(rowValue("缓存 buffers")).toBe("0.2GB");
    expect(rowValue("缓存 cached")).toBe("0.5GB");
    expect(rowValue("交换分区")).toBe("1.0GB / 2.0GB");
    expect(rowValue("总量")).toBe("4.0GB");
    // 进程列表也在同一张卡里
    expect(screen.getByText("nginx")).toBeTruthy();
    expect(screen.getByText("12.5%")).toBeTruthy();
  });

  it("CPU 详情：核心数、每核数值、负载", async () => {
    renderBar();
    fireEvent.mouseOver(screen.getByTitle("CPU（悬停看每个核心）"));
    expect(await screen.findByText("CPU（2 核心）")).toBeTruthy();
    expect(screen.getByText("核心 0")).toBeTruthy();
    expect(screen.getByText("90%")).toBeTruthy();
    expect(screen.getByText("核心 1")).toBeTruthy();
    expect(screen.getByText("10%")).toBeTruthy();
    expect(
      screen.getByText(/负载（1\/5\/15 分钟）：0\.50 \/ 0\.25 \/ 0\.10/),
    ).toBeTruthy();
  });

  it("CPU 详情：有型号时标题带型号，全称放在 title 里", async () => {
    const model = "Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz";
    renderBar({ ...FIXTURE, cpu_model: model });
    fireEvent.mouseOver(screen.getByTitle("CPU（悬停看每个核心）"));
    const heading = await screen.findByText(`CPU · ${model}（2 核心）`);
    expect(heading.getAttribute("title")).toBe(`CPU · ${model}（2 核心）`);
    expect(heading.className).toContain("truncate");
  });

  it("磁盘详情：已用 / 总量 / 使用率", async () => {
    renderBar();
    fireEvent.mouseOver(screen.getByTitle("根分区"));
    expect(await screen.findByText("根分区")).toBeTruthy();
    expect(rowValue("已用")).toBe("10.0GB");
    expect(rowValue("总量")).toBe("40.0GB");
    expect(rowValue("使用率")).toBe("25%");
  });

  it("网速详情：每块网卡的速率和系统信息", async () => {
    renderBar();
    fireEvent.mouseOver(screen.getByTitle("网速（悬停看每块网卡）"));
    expect(await screen.findByText("网络")).toBeTruthy();
    const eth0Row = screen.getByText("eth0").parentElement;
    expect(eth0Row?.textContent).toContain("2.0KB/s");
    expect(eth0Row?.textContent).toContain("1.0MB/s");
    expect(rowValue("系统")).toBe("Debian GNU/Linux 12");
    expect(rowValue("内核")).toBe("6.1.0-test");
    expect(rowValue("在线时长")).toBe("1 天 1 小时");
  });
});

describe("ServerStatsBar 窄了逐级收起", () => {
  // jsdom 不排版：把读数区的内容宽度按"每个读数 itemWidth（默认 100px）"算，可用宽度 250px
  let itemWidth = 100;
  function fakeLayout() {
    itemWidth = 100;
    const scroll = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollWidth");
    const client = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    Object.defineProperty(HTMLElement.prototype, "scrollWidth", {
      configurable: true,
      get(this: HTMLElement) {
        return this.dataset.collapseLevel === undefined ? 0 : this.querySelectorAll("button").length * itemWidth;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get(this: HTMLElement) {
        return this.dataset.collapseLevel === undefined ? 0 : 250;
      },
    });
    return () => {
      if (scroll) Object.defineProperty(HTMLElement.prototype, "scrollWidth", scroll);
      if (client) Object.defineProperty(HTMLElement.prototype, "clientWidth", client);
    };
  }

  it("放不下就按 运行时长 → 网速 → 磁盘 的顺序收，CPU 和概览入口一直在", () => {
    const restore = fakeLayout();
    try {
      render(<ServerStatsBar stats={FIXTURE} connected onToggleOverview={() => {}} />);
      expect(screen.getByTitle("CPU（悬停看每个核心）")).toBeTruthy();
      expect(screen.getByTitle("内存（悬停看明细和占用最多的进程）")).toBeTruthy();
      expect(screen.queryByTitle("根分区")).toBeNull();
      expect(screen.queryByTitle("网速（悬停看每块网卡）")).toBeNull();
      expect(screen.queryByTitle("已运行（开机以来）")).toBeNull();
      expect(screen.getByText("概览")).toBeTruthy();
    } finally {
      restore();
    }
  });

  it("读数变短、放得下了：不用改宽度也重新展开", () => {
    const restore = fakeLayout();
    try {
      const { rerender } = render(<ServerStatsBar stats={FIXTURE} connected onToggleOverview={() => {}} />);
      expect(screen.queryByTitle("根分区")).toBeNull();
      itemWidth = 40;
      rerender(<ServerStatsBar stats={{ ...FIXTURE }} connected onToggleOverview={() => {}} />);
      expect(screen.getByTitle("根分区")).toBeTruthy();
      expect(screen.getByTitle("已运行（开机以来）")).toBeTruthy();
    } finally {
      restore();
    }
  });

  it("收起的读数超阈值：概览入口给出提示", () => {
    const restore = fakeLayout();
    try {
      render(
        <ServerStatsBar stats={{ ...FIXTURE, disk_percent: 97 }} connected onToggleOverview={() => {}} />,
      );
      expect(screen.queryByTitle("根分区")).toBeNull();
      expect(screen.getByTitle("有收起的读数超过阈值，点开概览看")).toBeTruthy();
    } finally {
      restore();
    }
  });
});

describe("formatNetSpeed", () => {
  it("自动换算 B/s、KB/s、MB/s、GB/s", () => {
    expect(formatNetSpeed(0)).toBe("0B/s");
    expect(formatNetSpeed(999)).toBe("999B/s");
    expect(formatNetSpeed(1024)).toBe("1.0KB/s");
    expect(formatNetSpeed(1500)).toBe("1.5KB/s");
    expect(formatNetSpeed(1024 * 1024)).toBe("1.0MB/s");
    expect(formatNetSpeed(3.5 * 1024 * 1024)).toBe("3.5MB/s");
    expect(formatNetSpeed(2.25 * 1024 * 1024 * 1024)).toBe("2.3GB/s");
  });

  it("负数 / 无穷按 0 处理", () => {
    expect(formatNetSpeed(-5)).toBe("0B/s");
    expect(formatNetSpeed(Number.NaN)).toBe("0B/s");
  });
});

describe("formatMibAsG", () => {
  it("MiB 进位换算成 GB（1024 进制），缺数据显示 --", () => {
    expect(formatMibAsG(1024)).toBe("1.0GB");
    expect(formatMibAsG(15947)).toBe("15.6GB");
    expect(formatMibAsG(0)).toBe("0.0GB");
    expect(formatMibAsG(null)).toBe("--");
  });
});

describe("formatCpuHeading", () => {
  it("有型号：CPU · 型号（N 核心），连续空白压成一个", () => {
    expect(formatCpuHeading({ cpu_cores: 1, cpu_model: "Intel(R)  Xeon(R) Gold 6148 " })).toBe(
      "CPU · Intel(R) Xeon(R) Gold 6148（1 核心）",
    );
  });

  it("旧数据没有字段 / null / 空串都退回不带型号", () => {
    expect(formatCpuHeading({ cpu_cores: 2 })).toBe("CPU（2 核心）");
    expect(formatCpuHeading({ cpu_cores: 2, cpu_model: null })).toBe("CPU（2 核心）");
    expect(formatCpuHeading({ cpu_cores: null, cpu_model: "  " })).toBe("CPU（? 核心）");
  });
});

describe("formatUptime", () => {
  it("在线时长按天 / 小时 / 分钟说人话", () => {
    expect(formatUptime(90)).toBe("1 分钟");
    expect(formatUptime(3600)).toBe("1 小时 0 分");
    expect(formatUptime(3661)).toBe("1 小时 1 分");
    expect(formatUptime(86400 * 3 + 7200)).toBe("3 天 2 小时");
    expect(formatUptime(null)).toBe("0 分钟");
  });
});

describe("formatTraffic", () => {
  it("字节换成合适的单位", () => {
    expect(formatTraffic(0)).toBe("0B");
    expect(formatTraffic(512)).toBe("512B");
    expect(formatTraffic(1536)).toBe("1.5KB");
    expect(formatTraffic(5 * 1024 ** 3)).toBe("5.0GB");
    expect(formatTraffic(2.5 * 1024 ** 4)).toBe("2.5TB");
    expect(formatTraffic(Number.NaN)).toBe("0B");
  });
});
