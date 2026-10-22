//! SFTP 标签顶上的一条：这个标签连的是哪台主机，下拉换一台（同一个标签里重连）。

import { Server } from "lucide-react";

import { useVault } from "../../stores/vault";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";

export default function SftpHostBar({
  hostId,
  busy,
  onPick,
}: {
  /** 0 = 还没选 */
  hostId: number;
  busy: boolean;
  onPick: (host: { id: number; name: string }) => void;
}) {
  const hosts = useVault((state) => state.hosts);

  return (
    <div
      data-section="sftp-host-bar"
      className="flex h-9 shrink-0 items-center gap-2 border-b border-border/60 px-3 text-xs"
    >
      <Server size={13} className="text-sky-400" />
      <span className="text-muted-foreground">主机</span>
      <Select
        value={hostId ? String(hostId) : undefined}
        disabled={busy}
        onValueChange={(value) => {
          const host = hosts.find((item) => String(item.id) === value);
          if (host) onPick({ id: host.id, name: host.name });
        }}
      >
        <SelectTrigger className="h-7 w-56 text-xs" aria-label="SFTP 主机">
          <SelectValue placeholder={hosts.length ? "选择主机…" : "主机库里还没有主机"} />
        </SelectTrigger>
        <SelectContent>
          {hosts.map((host) => (
            <SelectItem key={host.id} value={String(host.id)} className="text-xs">
              {host.name}
              <span className="ml-2 text-muted-foreground">
                {host.username}@{host.host}
              </span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {busy && <span className="text-muted-foreground">连接中…</span>}
    </div>
  );
}
