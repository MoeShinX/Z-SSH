//! 终端界面主机树底部的只读属性面板（类似 Xshell 会话管理器下面的那块表格）。
//! 只显示名称、地址、用户名、协议、端口，有跳板机 / 指定代理时再加一行名字；不显示任何凭据。

import { Copy, X } from "lucide-react";

import type { Host, Proxy } from "../../api/vault";
import { cn } from "../../lib/utils";
import { normalizeHostInput } from "../../state/hostAddress";
import { Button } from "../ui/button";

export interface HostInfoRow {
  key: string;
  label: string;
  value: string;
}

/** 面板里的行。没有跳板机 / 没指定代理就不出对应的行（跟随默认代理的看不出具体是谁，也不显示）。 */
export function hostInfoRows(host: Host, hosts: Host[], proxies: Proxy[]): HostInfoRow[] {
  const rows: HostInfoRow[] = [
    { key: "name", label: "名称", value: host.name },
    { key: "address", label: "主机", value: normalizeHostInput(host.host) },
    { key: "user", label: "用户名", value: host.username },
    { key: "protocol", label: "协议", value: "SSH" },
    { key: "port", label: "端口", value: String(host.port) },
  ];
  if (host.jump_host_id !== null) {
    const jump = hosts.find((item) => item.id === host.jump_host_id);
    if (jump) rows.push({ key: "jump", label: "跳板机", value: jump.name });
  }
  if (host.proxy_mode === "custom" && host.proxy_id !== null) {
    const proxy = proxies.find((item) => item.id === host.proxy_id);
    if (proxy) rows.push({ key: "proxy", label: "代理", value: proxy.name });
  }
  return rows;
}

export default function HostInfoPanel({
  host,
  hosts,
  proxies,
  onCopyAddress,
  onClose,
}: {
  host: Host;
  hosts: Host[];
  proxies: Proxy[];
  onCopyAddress: () => void;
  onClose: () => void;
}) {
  const rows = hostInfoRows(host, hosts, proxies);
  return (
    <section
      data-section="host-info"
      aria-label="主机属性"
      // 高度固定（放得下 7 行：基本 5 行 + 跳板机 + 代理）：切换主机或有无跳板机 / 代理行时，上面的树不会跟着跳
      className="flex h-[184px] shrink-0 flex-col border-t border-border/60 bg-background text-xs"
    >
      <header className="flex h-6 shrink-0 items-center justify-between pl-3 pr-1 text-muted-foreground">
        <span className="font-medium">属性</span>
        <Button size="icon-sm" variant="ghost" className="h-5 w-5" title="收起" aria-label="收起属性面板" onClick={onClose}>
          <X size={12} />
        </Button>
      </header>
      {/* 像 Xshell 会话管理器那块表格：外框 + 行间横线 + 标题列和值之间一条竖线，标题列带一点底色 */}
      <div className="mx-2 mb-2 min-h-0 flex-1 overflow-y-auto">
        <dl className="overflow-hidden rounded-sm border border-border/60">
          {rows.map((row) => (
            <div
              key={row.key}
              className="grid h-[22px] grid-cols-[4rem_1fr] items-stretch border-b border-border/60 last:border-b-0"
            >
              <dt className="flex items-center truncate border-r border-border/60 bg-muted/40 px-2 text-muted-foreground">
                {row.label}
              </dt>
              <dd
                title={row.value}
                className={cn("flex min-w-0 items-center gap-1 px-2", row.key === "address" && "group")}
              >
                <span className="truncate">{row.value}</span>
                {row.key === "address" && (
                  <button
                    type="button"
                    title="复制地址"
                    aria-label="复制地址"
                    onClick={onCopyAddress}
                    className="flex size-4 shrink-0 cursor-pointer items-center justify-center rounded text-muted-foreground opacity-0 hover:bg-accent hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
                  >
                    <Copy size={12} />
                  </button>
                )}
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </section>
  );
}
