//! 空白终端标签里的"选主机"：搜索 + 主机列表。点一行就在这个标签里连上；每行的 SFTP 按钮
//! 另开一个 SFTP 标签。不依赖左边的主机树（侧栏收起时也能选）。

import { FolderTree, Search, Server } from "lucide-react";
import { useMemo, useState } from "react";

import type { Host } from "../../api/vault";
import { compareNames } from "../../state/hostTree";
import { cn } from "../../lib/utils";
import { useVault } from "../../stores/vault";

export function filterHosts(hosts: Host[], query: string): Host[] {
  const text = query.trim().toLowerCase();
  const sorted = [...hosts].sort((a, b) => compareNames(a.name, b.name));
  if (!text) return sorted;
  return sorted.filter((host) =>
    [host.name, host.host, host.username, ...host.tags].some((field) => field.toLowerCase().includes(text)),
  );
}

export default function EmptyTabLauncher({
  onConnect,
  onOpenSftp,
}: {
  onConnect: (host: Host) => void;
  onOpenSftp: (host: Host) => void;
}) {
  const hosts = useVault((state) => state.hosts);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const shown = useMemo(() => filterHosts(hosts, query), [hosts, query]);
  const current = Math.min(cursor, Math.max(0, shown.length - 1));

  return (
    <div
      data-section="empty-tab-launcher"
      className="absolute inset-0 z-10 flex items-start justify-center overflow-y-auto pt-[12vh]"
      // 和窗格同一个底色，盖住后面空终端的光标
      style={{ background: "inherit" }}
      // 点在这里不要让终端抢焦点
      onMouseDown={(event) => event.stopPropagation()}
    >
      <div className="w-full max-w-md space-y-3 px-4">
        <p className="text-sm font-medium">连接到主机</p>
        <div className="relative">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <input
            autoFocus
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setCursor(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setCursor((value) => Math.min(value + 1, shown.length - 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setCursor((value) => Math.max(value - 1, 0));
              } else if (event.key === "Enter" && shown[current]) {
                event.preventDefault();
                onConnect(shown[current]);
              }
            }}
            placeholder="搜索主机名、地址、标签，回车连接"
            className="h-9 w-full rounded-md border border-input bg-background pl-9 pr-3 text-sm outline-none focus:border-ring"
          />
        </div>
        {hosts.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">
            主机库里还没有主机，到「主机」页新建一台。
          </p>
        ) : shown.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">没有匹配的主机</p>
        ) : (
          <ul className="max-h-[50vh] space-y-0.5 overflow-y-auto">
            {shown.map((host, index) => (
              <li key={host.id}>
                <div
                  role="button"
                  tabIndex={-1}
                  data-host={host.name}
                  onMouseEnter={() => setCursor(index)}
                  onClick={() => onConnect(host)}
                  className={cn(
                    "group flex cursor-pointer items-center gap-2 rounded-md px-3 py-2 text-sm",
                    index === current ? "bg-accent text-accent-foreground" : "hover:bg-accent/60",
                  )}
                >
                  <Server size={14} className="shrink-0 text-sky-400" />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate">{host.name}</span>
                    <span className="block truncate font-mono text-[11px] text-muted-foreground">
                      {host.username}@{host.host}:{host.port}
                    </span>
                  </span>
                  <button
                    type="button"
                    title="打开 SFTP（新标签）"
                    onClick={(event) => {
                      event.stopPropagation();
                      onOpenSftp(host);
                    }}
                    className="flex shrink-0 items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground opacity-70 hover:bg-background hover:text-foreground hover:opacity-100"
                  >
                    <FolderTree size={12} /> SFTP
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
