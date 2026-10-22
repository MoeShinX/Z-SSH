//! 服务器概览（点底部状态栏打开）：CPU / 内存 / 磁盘 / 负载四个圆环、网速曲线、每个核心、
//! 系统信息、全部分区、网卡、内存占用前几的进程。
//!
//! 数据就是状态栏那条采集（默认 3 秒一次），不额外开连接、不额外跑命令；面板关着不渲染。
//! 网速曲线的历史在 App 里按会话记（最近几分钟），面板打开时直接画。

import { ArrowDownToLine, ArrowUpFromLine, Gauge, X } from "lucide-react";

import type { ServerStats } from "../../api/session";
import { cn } from "../../lib/utils";
import { ResizeHandle, usePanelWidth } from "../ui/resize-handle";
import { formatMibAsG, formatNetSpeed, formatTraffic, formatUptime } from "./ServerStatsBar";

/** 一个网速点（字节/秒）。 */
export interface NetPoint {
  rx: number;
  tx: number;
}

/** 概览面板的默认宽度。 */
export const OVERVIEW_WIDTH_DEFAULT = 360;

/** 网速曲线最多记这么多个点（3 秒一次 ≈ 6 分钟）。 */
export const NET_HISTORY_MAX = 120;

/** 追加一个点，超过上限丢最旧的。返回新数组（不改原数组）。 */
export function pushNetPoint(history: readonly NetPoint[], point: NetPoint): NetPoint[] {
  const next = history.length >= NET_HISTORY_MAX ? history.slice(1) : history.slice();
  next.push(point);
  return next;
}

/** 用量颜色：80% 黄、95% 红，其余用主题强调色。 */
function ringColor(percent: number | null): string {
  if (percent === null) return "hsl(var(--muted-foreground))";
  if (percent >= 95) return "rgb(239 68 68)";
  if (percent >= 80) return "rgb(245 158 11)";
  return "hsl(var(--primary))";
}

function Ring({
  label,
  percent,
  value,
  sub,
}: {
  label: string;
  percent: number | null;
  value: string;
  sub: string;
}) {
  const radius = 26;
  const circumference = 2 * Math.PI * radius;
  const shown = Math.max(0, Math.min(100, percent ?? 0));
  return (
    <div className="flex flex-col items-center gap-1" data-ring={label}>
      <div className="relative h-16 w-16">
        <svg viewBox="0 0 64 64" className="h-16 w-16 -rotate-90">
          <circle cx="32" cy="32" r={radius} fill="none" strokeWidth="6" stroke="hsl(var(--muted))" />
          <circle
            cx="32"
            cy="32"
            r={radius}
            fill="none"
            strokeWidth="6"
            strokeLinecap="round"
            stroke={ringColor(percent)}
            strokeDasharray={circumference}
            strokeDashoffset={circumference * (1 - shown / 100)}
          />
        </svg>
        <span className="absolute inset-0 flex items-center justify-center text-sm font-medium">{value}</span>
      </div>
      <span className="text-[11px] font-medium tracking-wide text-muted-foreground">{label}</span>
      <span className="max-w-20 truncate text-[10px] text-muted-foreground" title={sub}>
        {sub}
      </span>
    </div>
  );
}

/** 网速曲线：下行实线、上行虚线，纵轴按这段时间的最大值缩放。 */
function NetChart({ history }: { history: readonly NetPoint[] }) {
  const width = 320;
  const height = 56;
  if (history.length < 2) {
    return <div className="flex h-14 items-center text-[11px] text-muted-foreground">正在积累数据…</div>;
  }
  const max = Math.max(1, ...history.map((point) => Math.max(point.rx, point.tx)));
  // 点少的时候按 30 个点的宽度铺开（不然几个点挤在最右边一小条），多了再往左延伸
  const slots = Math.max(30, history.length);
  const step = width / (slots - 1);
  const offset = width - step * (history.length - 1);
  const line = (pick: (point: NetPoint) => number) =>
    history
      .map((point, index) => {
        const x = offset + index * step;
        const y = height - 2 - (pick(point) / max) * (height - 4);
        return `${index === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  return (
    <svg viewBox={`0 0 ${width} ${height}`} className="h-14 w-full" preserveAspectRatio="none" data-section="net-chart">
      <path d={line((point) => point.rx)} fill="none" stroke="rgb(52 211 153)" strokeWidth="1.5" />
      <path
        d={line((point) => point.tx)}
        fill="none"
        stroke="rgb(56 189 248)"
        strokeWidth="1.2"
        strokeDasharray="3 2"
      />
    </svg>
  );
}

function Section({ title, extra, children }: { title: string; extra?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="space-y-2 border-t border-border/60 px-4 py-3">
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span>{title}</span>
        {extra}
      </div>
      {children}
    </section>
  );
}

function Bar({ percent }: { percent: number }) {
  return (
    <div className="h-1.5 overflow-hidden rounded-full bg-muted">
      <div
        className="h-full rounded-full"
        style={{ width: `${Math.max(0, Math.min(100, percent))}%`, background: ringColor(percent) }}
      />
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-[11px] text-muted-foreground">{label}</div>
      <div className="truncate text-xs" title={typeof children === "string" ? children : undefined}>
        {children}
      </div>
    </div>
  );
}

export default function ServerOverviewPanel({
  stats,
  history,
  title,
  onClose,
}: {
  stats: ServerStats | null;
  history: readonly NetPoint[];
  /** 主机名 / 标签 */
  title: string;
  onClose: () => void;
}) {
  // 拖左边缘调宽（双击恢复默认）；面板盖在终端上，宽度变了终端不用重排
  const { width, bounds, onResize, onCommit, onReset } = usePanelWidth(
    "aside",
    "layout.overview_width",
    OVERVIEW_WIDTH_DEFAULT,
  );
  const memPercent =
    stats && stats.mem_total_mib && stats.mem_used_mib !== null
      ? Math.round((stats.mem_used_mib / stats.mem_total_mib) * 100)
      : null;
  const cores = stats?.cpu_cores ?? null;
  // 负载按核心数折成百分比：1 核跑满 = 100%
  const loadPercent =
    stats?.load_1m !== null && stats?.load_1m !== undefined && cores
      ? Math.round((stats.load_1m / cores) * 100)
      : null;

  return (
    <aside
      data-section="server-overview"
      className="relative flex h-full flex-col border-l border-border bg-card text-foreground shadow-lg"
      style={{ width }}
    >
      <ResizeHandle width={width} bounds={bounds} onResize={onResize} onCommit={onCommit} onReset={onReset} />
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-border/60 px-4">
        <Gauge size={14} className="text-primary" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium" title={title}>
          {title || "服务器概览"}
        </span>
        <button
          type="button"
          title="关闭概览"
          onClick={onClose}
          className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <X size={14} />
        </button>
      </header>

      {!stats ? (
        <div className="p-4 text-sm text-muted-foreground">状态采集等待中…</div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="grid grid-cols-4 gap-1 px-3 py-4">
            <Ring
              label="CPU"
              percent={stats.cpu_percent}
              value={stats.cpu_percent === null ? "--" : `${stats.cpu_percent}%`}
              sub={cores ? `${cores} 核` : ""}
            />
            <Ring
              label="内存"
              percent={memPercent}
              value={memPercent === null ? "--" : `${memPercent}%`}
              sub={`${formatMibAsG(stats.mem_used_mib)}/${formatMibAsG(stats.mem_total_mib)}`}
            />
            <Ring
              label="磁盘"
              percent={stats.disk_percent}
              value={stats.disk_percent === null ? "--" : `${stats.disk_percent}%`}
              sub={`${formatMibAsG(stats.disk_used_mib, 0)}/${formatMibAsG(stats.disk_total_mib, 0)}`}
            />
            <Ring
              label="负载"
              percent={loadPercent}
              value={loadPercent === null ? "--" : `${loadPercent}%`}
              sub={stats.load_1m === null ? "" : stats.load_1m.toFixed(2)}
            />
          </div>

          <Section
            title="网络"
            extra={
              <span className="flex items-center gap-2">
                <span className="flex items-center gap-0.5 text-emerald-400">
                  <ArrowDownToLine size={10} />
                  {formatNetSpeed(stats.net_rx_speed)}
                </span>
                <span className="flex items-center gap-0.5 text-sky-400">
                  <ArrowUpFromLine size={10} />
                  {formatNetSpeed(stats.net_tx_speed)}
                </span>
              </span>
            }
          >
            <NetChart history={history} />
          </Section>

          {stats.per_core_percent.length > 0 && (
            <Section title="核心" extra={<span>{stats.per_core_percent.length}</span>}>
              <div className="grid max-h-40 grid-cols-[repeat(auto-fill,minmax(22px,1fr))] gap-1 overflow-y-auto">
                {stats.per_core_percent.map((percent, index) => (
                  <div
                    key={index}
                    title={`核心 ${index}：${percent === null ? "--" : `${percent}%`}`}
                    className="relative h-8 overflow-hidden rounded-sm bg-muted"
                  >
                    <div
                      className="absolute inset-x-0 bottom-0"
                      style={{ height: `${percent ?? 0}%`, background: ringColor(percent) }}
                    />
                  </div>
                ))}
              </div>
            </Section>
          )}

          <Section title="系统">
            <div className="grid grid-cols-2 gap-x-3 gap-y-2">
              <Field label="系统">{stats.os_name || "--"}</Field>
              <Field label="内核">{stats.kernel || "--"}</Field>
              <Field label="运行时间">{stats.uptime_secs === null ? "--" : formatUptime(stats.uptime_secs)}</Field>
              <Field label="负载 1 / 5 / 15m">
                {[stats.load_1m, stats.load_5m, stats.load_15m]
                  .map((value) => (value === null ? "--" : value.toFixed(2)))
                  .join(" / ")}
              </Field>
              <Field label="交换空间">
                {stats.swap_total_mib
                  ? `${formatMibAsG(stats.swap_used_mib)}/${formatMibAsG(stats.swap_total_mib)}`
                  : "--"}
              </Field>
              {stats.cpu_model && <Field label="CPU">{stats.cpu_model}</Field>}
            </div>
          </Section>

          {(stats.mounts?.length ?? 0) > 0 && (
            <Section title="存储" extra={<span>{stats.mounts!.length}</span>}>
              <div className="space-y-2.5">
                {stats.mounts!.map((mount) => (
                  <div key={`${mount.source}-${mount.mount}`} className="space-y-1" data-mount={mount.mount}>
                    <div className="flex items-center justify-between gap-2 text-xs">
                      <span className="truncate font-medium" title={mount.mount}>
                        {mount.mount}
                      </span>
                      <span className="shrink-0">{mount.percent}%</span>
                    </div>
                    <Bar percent={mount.percent} />
                    <div className="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
                      <span className="truncate" title={mount.source}>
                        {mount.fs_type} · {mount.source}
                      </span>
                      <span className="shrink-0">
                        {formatMibAsG(mount.used_mib)} / {formatMibAsG(mount.total_mib)}
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            </Section>
          )}

          {stats.net_interfaces.length > 0 && (
            <Section title="网卡">
              <div className="space-y-1">
                {stats.net_interfaces.map((iface) => (
                  <div key={iface.name} className="space-y-0.5">
                    <div className="flex items-center gap-2 text-xs">
                      <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
                      <span className="min-w-0 flex-1 truncate">{iface.name}</span>
                      <span className="text-emerald-400">↓ {formatNetSpeed(iface.rx_speed)}</span>
                      <span className="text-sky-400">↑ {formatNetSpeed(iface.tx_speed)}</span>
                    </div>
                    {iface.rx_total !== undefined && iface.tx_total !== undefined && (
                      <div
                        data-section="iface-total"
                        title="网卡启用以来（一般就是开机以来）收发的总量，服务器重启后从零算起"
                        className="flex items-center justify-between gap-2 pl-3.5 text-[10px] text-muted-foreground"
                      >
                        <span>开机以来</span>
                        <span>
                          ↓ {formatTraffic(iface.rx_total)} · ↑ {formatTraffic(iface.tx_total)}
                        </span>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </Section>
          )}

          {stats.top_processes.length > 0 && (
            <Section title="内存占用进程">
              <table className="w-full text-xs">
                <thead>
                  <tr className="text-left text-[11px] text-muted-foreground">
                    <th className="w-16 py-1 font-normal">PID</th>
                    <th className="py-1 font-normal">命令</th>
                    <th className="w-20 py-1 text-right font-normal">内存</th>
                  </tr>
                </thead>
                <tbody>
                  {stats.top_processes.map((proc) => (
                    <tr key={proc.pid} className="border-t border-border/40">
                      <td className="py-1 font-mono text-[11px] text-muted-foreground">{proc.pid}</td>
                      <td className="max-w-0 truncate py-1 font-mono text-[11px]" title={proc.command}>
                        {proc.command}
                      </td>
                      <td className="py-1">
                        <div className="relative ml-auto h-4 w-16 overflow-hidden rounded-sm bg-muted">
                          <div
                            className={cn("absolute inset-y-0 left-0 bg-primary/30")}
                            style={{ width: `${Math.min(100, proc.mem_percent)}%` }}
                          />
                          <span className="relative block pr-1 text-right text-[10px] leading-4">
                            {proc.mem_percent.toFixed(1)}%
                          </span>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </Section>
          )}
        </div>
      )}
    </aside>
  );
}
