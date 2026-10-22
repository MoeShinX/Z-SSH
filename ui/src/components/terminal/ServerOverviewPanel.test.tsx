/** @vitest-environment jsdom */
//! 服务器概览：四个圆环、分区、进程、网速曲线；没数据时提示等待；网速历史有上限。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ServerStats } from "../../api/session";
import ServerOverviewPanel, { NET_HISTORY_MAX, pushNetPoint } from "./ServerOverviewPanel";

afterEach(cleanup);

const STATS: ServerStats = {
  cpu_percent: 12,
  per_core_percent: [10, 14],
  cpu_cores: 2,
  mem_total_mib: 4096,
  mem_used_mib: 1024,
  mem_free_mib: 2048,
  mem_buffers_mib: 512,
  mem_cached_mib: 512,
  swap_total_mib: 0,
  swap_used_mib: 0,
  disk_total_mib: 40960,
  disk_used_mib: 10240,
  disk_percent: 25,
  net_rx_speed: 2048,
  net_tx_speed: 1024,
  net_interfaces: [{ name: "eth0", rx_speed: 2048, tx_speed: 1024 }],
  mounts: [
    { mount: "/", fs_type: "ext4", source: "/dev/vda1", used_mib: 10240, total_mib: 40960, percent: 25 },
    { mount: "/data", fs_type: "xfs", source: "/dev/vdb", used_mib: 100, total_mib: 1000, percent: 10 },
  ],
  top_processes: [{ pid: "101", mem_percent: 9.1, command: "dockerd" }],
  hostname: "web-1",
  os_name: "Debian GNU/Linux 12",
  kernel: "6.1.0",
  uptime_secs: 90061,
  load_1m: 0.5,
  load_5m: 0.4,
  load_15m: 0.3,
};

describe("服务器概览", () => {
  it("圆环、分区、进程、系统信息都显示；点关闭回调", () => {
    const onClose = vi.fn();
    render(
      <ServerOverviewPanel
        stats={STATS}
        history={[{ rx: 1, tx: 2 }, { rx: 3, tx: 4 }]}
        title="web-1"
        onClose={onClose}
      />,
    );
    expect(document.querySelector('[data-ring="CPU"]')?.textContent).toContain("12%");
    expect(document.querySelector('[data-ring="内存"]')?.textContent).toContain("25%");
    expect(document.querySelector('[data-ring="负载"]')?.textContent).toContain("25%");
    expect(document.querySelector('[data-mount="/data"]')?.textContent).toContain("xfs");
    expect(screen.getByText("dockerd")).toBeTruthy();
    expect(screen.getByText("Debian GNU/Linux 12")).toBeTruthy();
    expect(screen.getByText("1 天 1 小时")).toBeTruthy();
    expect(document.querySelector('[data-section="net-chart"]')).not.toBeNull();
    // 左边缘有拖拽把手，默认宽 360
    expect(document.querySelector('[data-section="server-overview"] [data-section="panel-resize-handle"]')).not.toBeNull();
    expect((document.querySelector('[data-section="server-overview"]') as HTMLElement).style.width).toBe("360px");
    fireEvent.click(screen.getByTitle("关闭概览"));
    expect(onClose).toHaveBeenCalled();
  });

  it("还没数据时提示等待；旧后端没有分区字段也不报错", () => {
    const { rerender } = render(
      <ServerOverviewPanel stats={null} history={[]} title="" onClose={() => {}} />,
    );
    expect(screen.getByText("状态采集等待中…")).toBeTruthy();
    rerender(
      <ServerOverviewPanel stats={{ ...STATS, mounts: undefined }} history={[]} title="" onClose={() => {}} />,
    );
    expect(screen.getByText("正在积累数据…")).toBeTruthy();
  });

  it("网速历史超过上限丢最旧的", () => {
    let history = [] as { rx: number; tx: number }[];
    for (let i = 0; i < NET_HISTORY_MAX + 5; i += 1) history = pushNetPoint(history, { rx: i, tx: i });
    expect(history).toHaveLength(NET_HISTORY_MAX);
    expect(history[0].rx).toBe(5);
  });
});

describe("开机以来的流量", () => {
  const base = {
    cpu_percent: 2,
    per_core_percent: [2],
    cpu_cores: 1,
    mem_total_mib: 1024,
    mem_used_mib: 100,
    mem_free_mib: 900,
    mem_buffers_mib: 0,
    mem_cached_mib: 0,
    swap_total_mib: 0,
    swap_used_mib: 0,
    disk_total_mib: 1024,
    disk_used_mib: 10,
    disk_percent: 1,
    net_rx_speed: 0,
    net_tx_speed: 0,
    top_processes: [],
    hostname: "demo",
    os_name: "",
    kernel: "",
    uptime_secs: 60,
    load_1m: null,
    load_5m: null,
    load_15m: null,
  };

  it("每块网卡下面显示开机以来收发的总量", () => {
    const stats = {
      ...base,
      net_rx_total: 5 * 1024 ** 3,
      net_tx_total: 300 * 1024 ** 2,
      net_interfaces: [
        { name: "eth0", rx_speed: 0, tx_speed: 0, rx_total: 5 * 1024 ** 3, tx_total: 300 * 1024 ** 2 },
      ],
    } as unknown as ServerStats;
    render(<ServerOverviewPanel stats={stats} history={[]} title="demo" onClose={() => {}} />);
    const line = document.querySelector('[data-section="iface-total"]');
    expect(line?.textContent).toContain("开机以来");
    expect(line?.textContent).toContain("↓ 5.0GB");
    expect(line?.textContent).toContain("↑ 300.0MB");
  });

  it("旧后端没有累计字段：不显示这一行，也不报错", () => {
    const stats = {
      ...base,
      net_interfaces: [{ name: "eth0", rx_speed: 0, tx_speed: 0 }],
    } as unknown as ServerStats;
    render(<ServerOverviewPanel stats={stats} history={[]} title="demo" onClose={() => {}} />);
    expect(screen.getByText("eth0")).toBeTruthy();
    expect(document.querySelector('[data-section="iface-total"]')).toBeNull();
  });
});
