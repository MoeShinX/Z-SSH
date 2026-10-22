// Adapted from Netcatty components/terminal/TerminalServerStats.tsx（结构和观感）
//! 终端底部的服务器状态栏：CPU / 内存 / 磁盘 / 网速，悬停弹详情。
//!
//! 数据来自后端的推送（`server_stats` 事件，订阅 ID 认领），这里不做轮询。
//! 数值缺失显示"—"；CPU / 内存 80% 变黄、95% 变红。

import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Clock,
  Cpu,
  Gauge,
  HardDrive,
  MemoryStick,
} from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { cn } from "../../lib/utils";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/popover";
import type { ServerStats } from "../../api/session";

/** CPU / 内存变色阈值：黄 / 红。 */
const WARN = 80;
const CRITICAL = 95;

/** 网速自动换算：B/s → KB/s → MB/s → GB/s（和 Netcatty 的 formatNetSpeed 一致）。 */
export function formatNetSpeed(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return "0B/s";
  const units = ["B/s", "KB/s", "MB/s", "GB/s"];
  let value = bytesPerSec;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${Math.round(value)}${units[unit]}` : `${value.toFixed(1)}${units[unit]}`;
}

/** 累计流量（字节 → 123B / 4.5KB / 6.7MB / 8.9GB / 1.2TB）。 */
export function formatTraffic(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${Math.round(value)}${units[unit]}` : `${value.toFixed(1)}${units[unit]}`;
}

/** 内存/磁盘用的绝对值格式（MiB 输入 → x.xGB 显示）。 */
export function formatMibAsG(mib: number | null, digits = 1): string {
  if (mib === null || !Number.isFinite(mib)) return "--";
  return `${(mib / 1024).toFixed(digits)}GB`;
}

export function formatUptime(secs: number | null): string {
  const seconds = secs ?? 0;
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分钟`;
}

/** 用量颜色：80% 黄、95% 红。 */
function usageClass(percent: number | null): string {
  if (percent === null) return "";
  if (percent >= CRITICAL) return "text-red-400";
  if (percent >= WARN) return "text-amber-400";
  return "";
}

function MiniBar({ percent, className }: { percent: number; className?: string }) {
  // 一格宽的迷你条，宽度跟着用量走
  return (
    <span className="relative ml-0.5 inline-block h-1.5 w-9 shrink-0 overflow-hidden rounded-full bg-muted align-middle">
      <span
        className={cn(
          "absolute inset-y-0 left-0 rounded-full",
          percent >= CRITICAL ? "bg-red-500" : percent >= WARN ? "bg-amber-400" : "bg-muted-foreground/45",
          className,
        )}
        style={{ width: `${Math.min(100, Math.max(0, percent))}%` }}
      />
    </span>
  );
}

/** 悬停打开的浮层：详情在鼠标放上来时弹（任务卡要求"悬停弹出详情"，Radix 的
 * Popover 本身是点击驱动的，这里用 onMouseEnter/onMouseLeave 控制 open）。 */
function Hoverable({
  content,
  children,
  className,
}: {
  content: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        asChild
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        className={className}
      >
        {children}
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="start"
        className="w-auto p-3"
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        // 弹出层在 portal 里，但 React 事件仍会冒泡到状态栏：别让点详情也开关概览
        onClick={(event) => event.stopPropagation()}
      >
        {content}
      </PopoverContent>
    </Popover>
  );
}

interface ServerStatsBarProps {
  /** 来自 `onServerStats` 的最新数据；没连上是 null。 */
  stats: ServerStats | null;
  /** pane 的会话断开了没有。 */
  connected: boolean;
  /** 点状态栏：打开 / 关闭服务器概览。 */
  onToggleOverview?: () => void;
  overviewOpen?: boolean;
  /** pane 底部的剩余宽度太小就不显示（避免挤压终端）。 */
}

export default function ServerStatsBar({
  stats,
  connected,
  onToggleOverview,
  overviewOpen = false,
}: ServerStatsBarProps) {
  if (!connected) {
    return (
      <div className="flex h-6 shrink-0 items-center gap-2 border-t border-border/60 px-3 text-[11px] text-muted-foreground">
        已断开
      </div>
    );
  }
  if (!stats) {
    // 连上但还没收到第一次推送（或本次没采到）
    return (
      <div className="flex h-6 shrink-0 items-center gap-2 border-t border-border/60 px-3 text-[11px] text-muted-foreground">
        状态采集等待中…
      </div>
    );
  }

  return <LiveStatsBar stats={stats} onToggleOverview={onToggleOverview} overviewOpen={overviewOpen} />;
}

/**
 * 窄了按这个顺序收：先去迷你条，再收运行时长、网速、磁盘、内存，CPU 和概览入口一直在。
 * 收起的读数都在概览里（整条点一下就开）；收起的里面有超阈值的，概览入口跟着变色。
 */
const COLLAPSE_STEPS = 5;

function LiveStatsBar({
  stats,
  onToggleOverview,
  overviewOpen = false,
}: {
  stats: ServerStats;
  onToggleOverview?: () => void;
  overviewOpen?: boolean;
}) {
  const metricsRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [level, setLevel] = useState(0);

  // 按状态栏自己的宽度算（侧栏、AI 面板、分屏都会挤它），不看整个窗口
  useLayoutEffect(() => {
    const element = metricsRef.current?.parentElement;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.round(entry.contentRect.width)));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  // 宽度或读数变了都从头量：先全放，放不下再一级一级收（都在绘制前完成，不闪）。
  // 只看宽度的话，读数变短、空间够了也不会重新展开，得拖一下窗口才行。
  useLayoutEffect(() => setLevel(0), [width, stats]);
  useLayoutEffect(() => {
    const element = metricsRef.current;
    if (element && level < COLLAPSE_STEPS && element.scrollWidth > element.clientWidth + 1) {
      setLevel(level + 1);
    }
  });

  const showBars = level < 1;
  const showUptime = level < 2;
  const showNet = level < 3;
  const showDisk = level < 4;
  const showMem = level < 5;

  const cpu = stats.cpu_percent;
  const memPercent =
    stats.mem_total_mib !== null && stats.mem_used_mib !== null && stats.mem_total_mib > 0
      ? Math.round((stats.mem_used_mib / stats.mem_total_mib) * 100)
      : null;

  // 收起的读数里有超阈值的：概览入口借它的颜色（红优先）
  const hidden = [!showMem ? memPercent : null, !showDisk ? stats.disk_percent : null];
  const worst = Math.max(...hidden.map((value) => value ?? 0));
  const hiddenAlert = usageClass(worst >= WARN ? worst : null);

  return (
    <div
      data-section="server-stats-bar"
      onClick={onToggleOverview}
      title={onToggleOverview ? "点一下看服务器概览" : undefined}
      className={cn(
        // 普通读数用次级文字色、等宽数字（减少跳动时的左右晃）；超阈值的由 usageClass 上色
        "flex h-6 shrink-0 items-center gap-3 border-t border-border/60 px-3 text-[11px] text-muted-foreground tabular-nums select-none",
        onToggleOverview && "cursor-pointer",
        overviewOpen && "bg-accent",
      )}
    >
      <div
        ref={metricsRef}
        data-collapse-level={level}
        className="flex min-w-0 flex-1 items-center gap-3 overflow-hidden"
      >
        {/* CPU */}
        <Hoverable content={<CpuDetail stats={stats} />}>
          <button
            type="button"
            title="CPU（悬停看每个核心）"
            className="flex shrink-0 cursor-default items-center gap-1 hover:opacity-100"
          >
            <Cpu size={11} className={usageClass(cpu)} />
            <span className={usageClass(cpu)}>{cpu === null ? "--" : `${cpu}%`}</span>
            {showBars && cpu !== null && <MiniBar percent={cpu} />}
          </button>
        </Hoverable>

        {/* 内存 */}
        {showMem && (
          <Hoverable content={<MemoryDetail stats={stats} />}>
            <button
              type="button"
              title="内存（悬停看明细和占用最多的进程）"
              className="flex shrink-0 cursor-default items-center gap-1 hover:opacity-100"
            >
              <MemoryStick size={11} className={usageClass(memPercent)} />
              <span className={usageClass(memPercent)}>
                {stats.mem_used_mib === null || stats.mem_total_mib === null
                  ? "--"
                  : `${formatMibAsG(stats.mem_used_mib)}/${formatMibAsG(stats.mem_total_mib)}`}
              </span>
              {showBars && memPercent !== null && <MiniBar percent={memPercent} />}
            </button>
          </Hoverable>
        )}

        {/* 磁盘 */}
        {showDisk && (
          <Hoverable content={<DiskDetail stats={stats} />}>
            <button
              type="button"
              title="根分区"
              className="flex shrink-0 cursor-default items-center gap-1 hover:opacity-100"
            >
              <HardDrive size={11} className={usageClass(stats.disk_percent)} />
              <span className={usageClass(stats.disk_percent)}>
                {stats.disk_used_mib === null || stats.disk_total_mib === null
                  ? "--"
                  : `${formatMibAsG(stats.disk_used_mib, 0)}/${formatMibAsG(stats.disk_total_mib, 0)}`}
              </span>
              {showBars && stats.disk_percent !== null && <MiniBar percent={stats.disk_percent} />}
            </button>
          </Hoverable>
        )}

        {/* 网速（总上下行） */}
        {showNet && (
          <Hoverable content={<NetDetail stats={stats} />}>
            <button
              type="button"
              title="网速（悬停看每块网卡）"
              className="flex min-w-0 shrink-0 cursor-default items-center gap-1 hover:opacity-100"
            >
              <ArrowDownToLine size={10} className="flex-shrink-0 text-emerald-500/70" />
              <span className="min-w-[52px] truncate">{formatNetSpeed(stats.net_rx_speed)}</span>
              <ArrowUpFromLine size={10} className="flex-shrink-0 text-sky-500/70" />
              <span className="min-w-[52px] truncate">{formatNetSpeed(stats.net_tx_speed)}</span>
            </button>
          </Hoverable>
        )}

        {/* 开机以来的时间 */}
        {showUptime && stats.uptime_secs !== null && (
          <Hoverable content={<UptimeDetail stats={stats} />}>
            <button
              type="button"
              data-section="stats-uptime"
              title="已运行（开机以来）"
              className="flex shrink-0 cursor-default items-center gap-1 text-muted-foreground hover:opacity-100"
            >
              <Clock size={10} className="flex-shrink-0" />
              <span>运行 {formatUptime(stats.uptime_secs)}</span>
            </button>
          </Hoverable>
        )}

      </div>

      {onToggleOverview && (
        <span
          data-section="stats-overview-toggle"
          title={hiddenAlert ? "有收起的读数超过阈值，点开概览看" : undefined}
          className={cn(
            "flex shrink-0 items-center gap-1",
            hiddenAlert || (overviewOpen ? "text-primary" : "text-muted-foreground"),
          )}
        >
          <Gauge size={11} />
          概览
        </span>
      )}

    </div>
  );
}

/**
 * CPU 卡片标题：有型号时是"CPU · 型号（N 核心）"，没有（旧后端 / 远端拿不到）
 * 退回"CPU（N 核心）"。型号里的连续空白压成一个，空串当没有。
 */
export function formatCpuHeading(stats: Pick<ServerStats, "cpu_cores" | "cpu_model">): string {
  const model = stats.cpu_model?.replace(/\s+/g, " ").trim() ?? "";
  const cores = `（${stats.cpu_cores ?? "?"} 核心）`;
  return model ? `CPU · ${model}${cores}` : `CPU${cores}`;
}

/** 悬停：CPU 每核心 + 负载。 */
function CpuDetail({ stats }: { stats: ServerStats }) {
  const heading = formatCpuHeading(stats);
  return (
    <div className="space-y-2 text-xs">
      {/* 型号可能很长（服务器型号带频率等后缀）：截断，悬停 title 看全称 */}
      <div className="max-w-[22rem] truncate text-sm font-medium" title={heading}>
        {heading}
      </div>
      {stats.per_core_percent.length > 0 ? (
        <div className="grid max-h-52 grid-flow-col grid-rows-3 gap-x-4 gap-y-1.5 overflow-y-auto pr-1">
          {stats.per_core_percent.map((percent, index) => (
            <div key={index} className="flex min-w-24 items-center gap-2">
              <span className="w-12 text-[10px] text-muted-foreground">核心 {index}</span>
              <span
                className={cn(
                  "w-9 text-right text-[11px] font-medium",
                  usageClass(percent),
                )}
              >
                {percent === null ? "--" : `${percent}%`}
              </span>
              <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
                <div
                  className={cn(
                    "h-full rounded-full",
                    percent !== null && percent >= CRITICAL
                      ? "bg-red-500"
                      : percent !== null && percent >= WARN
                        ? "bg-amber-400"
                        : "bg-emerald-500",
                  )}
                  style={{ width: `${percent ?? 0}%` }}
                />
              </div>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-muted-foreground">暂无每个核心的数据</div>
      )}
      {stats.load_1m !== null && (
        <div className="text-[11px] text-muted-foreground">
          负载（1/5/15 分钟）：{[stats.load_1m, stats.load_5m, stats.load_15m]
            .map((item) => (item === null ? "--" : item.toFixed(2)))
            .join(" / ")}
        </div>
      )}
    </div>
  );
}

/** 悬停：内存明细 + swap + 占内存最多的进程。 */
function MemoryDetail({ stats }: { stats: ServerStats }) {
  return (
    <div className="space-y-2 text-xs">
      <div className="text-sm font-medium">内存</div>
      <div className="space-y-1 text-[11px]">
        <Row label="已用">{formatMibAsG(stats.mem_used_mib)}</Row>
        <Row label="可用">{formatMibAsG(stats.mem_free_mib)}</Row>
        <Row label="缓存 buffers">{formatMibAsG(stats.mem_buffers_mib)}</Row>
        <Row label="缓存 cached">{formatMibAsG(stats.mem_cached_mib)}</Row>
        {stats.swap_total_mib !== null && stats.swap_total_mib > 0 && (
          <Row label="交换分区">
            {formatMibAsG(stats.swap_used_mib)} / {formatMibAsG(stats.swap_total_mib)}
          </Row>
        )}
        <Row label="总量">{formatMibAsG(stats.mem_total_mib)}</Row>
      </div>
      {stats.top_processes.length > 0 && (
        <div className="space-y-1 pt-1">
          <div className="text-[11px] font-medium text-muted-foreground">占内存最多的进程</div>
          {stats.top_processes.slice(0, 10).map((proc) => (
            <div key={proc.pid} className="flex items-center gap-2 text-[10px]">
              <span className="w-16 text-right text-muted-foreground">
                {proc.mem_percent.toFixed(1)}%
              </span>
              <div className="h-1 flex-1 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-emerald-500"
                  style={{ width: `${Math.min(100, proc.mem_percent * 2)}%` }}
                />
              </div>
              <span className="w-36 shrink-0 truncate font-mono" title={proc.command}>
                {proc.command}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** 悬停：根分区。 */
function DiskDetail({ stats }: { stats: ServerStats }) {
  return (
    <div className="space-y-2 text-xs">
      <div className="text-sm font-medium">根分区</div>
      {stats.disk_total_mib !== null ? (
        <div className="space-y-1 text-[11px]">
          <Row label="已用">{formatMibAsG(stats.disk_used_mib)}</Row>
          <Row label="总量">
            {stats.disk_total_mib === null
              ? "--"
              : `${(stats.disk_total_mib / 1024).toFixed(1)}GB`}
          </Row>
          <Row label="使用率">
            {stats.disk_percent === null ? "--" : `${stats.disk_percent}%`}
          </Row>
        </div>
      ) : (
        <div className="text-muted-foreground">暂时没有数据</div>
      )}
    </div>
  );
}

/** 悬停：每块网卡的速率 + 系统信息。 */
function NetDetail({ stats }: { stats: ServerStats }) {
  return (
    <div className="space-y-2 text-xs">
      <div className="text-sm font-medium">网络</div>
      {stats.net_interfaces.length > 0 ? (
        <div className="space-y-1 max-h-40 overflow-y-auto">
          {stats.net_interfaces.map((iface) => (
            <div key={iface.name} className="flex min-w-52 items-center justify-between gap-4">
              <span className="font-mono text-[11px]">{iface.name}</span>
              <span className="flex items-center gap-1 text-emerald-400">
                <ArrowDownToLine size={9} />
                {formatNetSpeed(iface.rx_speed)}
              </span>
              <span className="flex items-center gap-1 text-sky-400">
                <ArrowUpFromLine size={9} />
                {formatNetSpeed(iface.tx_speed)}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-muted-foreground">没有网卡数据</div>
      )}
      {stats.net_rx_total !== undefined && stats.net_tx_total !== undefined && (
        <div
          data-section="net-total"
          title="所有网卡启用以来（一般就是开机以来）收发的总量，服务器重启后从零算起"
          className="flex items-center justify-between gap-4 text-[11px]"
        >
          <span className="text-muted-foreground">开机以来</span>
          <span className="flex items-center gap-1 text-emerald-400">
            <ArrowDownToLine size={9} />
            {formatTraffic(stats.net_rx_total)}
          </span>
          <span className="flex items-center gap-1 text-sky-400">
            <ArrowUpFromLine size={9} />
            {formatTraffic(stats.net_tx_total)}
          </span>
        </div>
      )}
      <div className="space-y-1 border-t border-border pt-2 text-[11px]">
        <Row label="系统">{stats.os_name || "--"}</Row>
        <Row label="内核">{stats.kernel || "--"}</Row>
        <Row label="在线时长">{formatUptime(stats.uptime_secs)}</Row>
      </div>
    </div>
  );
}

/** 悬停：运行时长、开机时间、系统、负载。 */
function UptimeDetail({ stats }: { stats: ServerStats }) {
  const bootAt =
    stats.uptime_secs === null ? null : new Date(Date.now() - stats.uptime_secs * 1000);
  return (
    <div className="space-y-1 text-[11px]">
      <Row label="已运行">{formatUptime(stats.uptime_secs)}</Row>
      {bootAt && (
        <Row label="开机于">
          {bootAt.toLocaleString("zh-CN", { hour12: false }).replace(/:\d{2}$/, "")}
        </Row>
      )}
      <Row label="系统">{stats.os_name || "--"}</Row>
      <Row label="内核">{stats.kernel || "--"}</Row>
      {stats.load_1m !== null && (
        <Row label="负载 1 / 5 / 15 分">
          {[stats.load_1m, stats.load_5m, stats.load_15m]
            .map((value) => (value === null ? "--" : value.toFixed(2)))
            .join(" / ")}
        </Row>
      )}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-medium">{children}</span>
    </div>
  );
}
