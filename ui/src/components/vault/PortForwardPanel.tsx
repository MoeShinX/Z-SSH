//! 端口转发面板：按主机列出规则，开 / 停、看状态，新建和修改。
//!
//! 规则只存本机。打开时可以只看某一台主机（从主机右键菜单进来），也可以看全部。

import { Play, Square } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  describeForward,
  forwardDelete,
  forwardList,
  forwardSave,
  forwardStart,
  forwardStatuses,
  forwardStop,
  formatBytes,
  onForwardStatus,
  type ForwardKind,
  type ForwardStatus,
  type PortForward,
  type PortForwardDraft,
} from "../../api/forwards";
import { cn } from "../../lib/utils";
import { useVault } from "../../stores/vault";
import {
  AsidePanel,
  AsidePanelContent,
  AsidePanelFooter,
  AsidePanelSection,
} from "../ui/aside-panel";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";

/** 有规则开着时多久刷新一次计数。 */
const POLL_MS = 1500;

interface PortForwardPanelProps {
  /** 只看这台主机；`null` 看全部。 */
  hostId: number | null;
  onClose: () => void;
  onError: (message: string) => void;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function emptyDraft(hostId: number): PortForwardDraft {
  return {
    id: null,
    host_id: hostId,
    name: "",
    kind: "local",
    bind_host: "127.0.0.1",
    bind_port: 8080,
    dest_host: "localhost",
    dest_port: 80,
    auto_start: false,
  };
}

function StatusBadge({ status }: { status: ForwardStatus | undefined }) {
  if (!status) return <Badge variant="outline">未开启</Badge>;
  if (status.phase === "starting") return <Badge variant="secondary">连接中…</Badge>;
  if (status.phase === "failed")
    return (
      <Badge variant="outline" className="border-destructive/60 text-destructive">
        出错
      </Badge>
    );
  return <Badge className="bg-emerald-600/90 text-white hover:bg-emerald-600/90">运行中</Badge>;
}

export default function PortForwardPanel({ hostId, onClose, onError }: PortForwardPanelProps) {
  const hosts = useVault((state) => state.hosts);
  const [rules, setRules] = useState<PortForward[]>([]);
  const [statuses, setStatuses] = useState<Map<number, ForwardStatus>>(new Map());
  const [draft, setDraft] = useState<PortForwardDraft | null>(null);
  const [busy, setBusy] = useState(false);

  // App 每次渲染都传一个新的 onError；放进 ref，免得刷新函数跟着变、反复重新订阅
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const report = useCallback((error: unknown) => onErrorRef.current(errorText(error)), []);

  const hostName = useMemo(() => new Map(hosts.map((host) => [host.id, host.name])), [hosts]);
  const visible = hostId === null ? rules : rules.filter((rule) => rule.host_id === hostId);

  const refreshStatuses = useCallback(async () => {
    try {
      const list = await forwardStatuses();
      setStatuses(new Map(list.map((item) => [item.id, item])));
    } catch (error) {
      report(error);
    }
  }, [report]);

  const refreshRules = useCallback(async () => {
    try {
      setRules(await forwardList());
    } catch (error) {
      report(error);
    }
  }, [report]);

  useEffect(() => {
    void refreshRules();
    void refreshStatuses();
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void onForwardStatus(() => void refreshStatuses()).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refreshRules, refreshStatuses]);

  // 只在有规则开着时轮询计数；都停着就不白跑
  const anyActive = [...statuses.values()].some((item) => item.phase !== "failed");
  useEffect(() => {
    if (!anyActive) return;
    const timer = window.setInterval(() => void refreshStatuses(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [anyActive, refreshStatuses]);

  const toggle = async (rule: PortForward) => {
    const status = statuses.get(rule.id);
    try {
      if (status && status.phase !== "failed") await forwardStop(rule.id);
      else await forwardStart(rule.id);
    } catch (error) {
      onError(errorText(error));
    }
    void refreshStatuses();
  };

  const save = async () => {
    if (!draft) return;
    setBusy(true);
    try {
      await forwardSave(draft);
      setDraft(null);
      await refreshRules();
    } catch (error) {
      onError(errorText(error));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!draft?.id) return;
    setBusy(true);
    try {
      await forwardDelete(draft.id);
      setDraft(null);
      await refreshRules();
      void refreshStatuses();
    } catch (error) {
      onError(errorText(error));
    } finally {
      setBusy(false);
    }
  };

  const newHostId = hostId ?? hosts[0]?.id ?? null;
  const title = hostId === null ? "端口转发" : `端口转发 · ${hostName.get(hostId) ?? ""}`;
  const set = <K extends keyof PortForwardDraft>(key: K, value: PortForwardDraft[K]) =>
    setDraft((current) => (current ? { ...current, [key]: value } : current));

  return (
    <AsidePanel
      title={title}
      subtitle={`${visible.length} 条`}
      onClose={onClose}
      actions={
        newHostId !== null && (
          <Button variant="ghost" size="sm" onClick={() => setDraft(emptyDraft(newHostId))}>
            新建
          </Button>
        )
      }
    >
      <AsidePanelContent>
        <div className="space-y-1">
          {visible.length === 0 && (
            <p className="px-1 py-2 text-sm text-muted-foreground">
              还没有转发规则。本地转发把服务器那边的端口（比如数据库）映射到本机；远程转发反过来，让服务器能连到本机这边。
            </p>
          )}
          {visible.map((rule) => {
            const status = statuses.get(rule.id);
            const active = status !== undefined && status.phase !== "failed";
            const editing = draft?.id === rule.id;
            return (
              <div
                key={rule.id}
                className={cn(
                  "rounded-md border px-3 py-2",
                  editing ? "border-primary" : "border-border/60",
                )}
              >
                <div className="flex items-center gap-2">
                  <button
                    className="min-w-0 flex-1 cursor-pointer text-left"
                    onClick={() => setDraft({ ...rule })}
                    title="编辑"
                  >
                    <span className="block truncate text-sm">
                      {rule.name || (rule.kind === "local" ? "本地转发" : "远程转发")}
                      {hostId === null && (
                        <span className="ml-2 text-xs text-muted-foreground">
                          {hostName.get(rule.host_id) ?? ""}
                        </span>
                      )}
                    </span>
                    <span className="block truncate font-mono text-[11px] text-muted-foreground">
                      {describeForward(rule)}
                    </span>
                  </button>
                  <StatusBadge status={status} />
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    title={active ? "停止" : "开启"}
                    onClick={() => void toggle(rule)}
                  >
                    {active ? <Square size={13} /> : <Play size={13} />}
                  </Button>
                </div>
                {status?.phase === "failed" && status.error && (
                  <p className="mt-1 text-xs text-destructive">{status.error}</p>
                )}
                {status?.phase === "running" && (
                  <p className="mt-1 text-[11px] text-muted-foreground">
                    {status.listening_port !== null && status.listening_port !== rule.bind_port
                      ? `服务器实际监听 ${status.listening_port} · `
                      : ""}
                    当前 {status.counters.active} 条连接 · 共 {status.counters.total} 条 · ↑
                    {formatBytes(status.counters.bytes_sent)} ↓
                    {formatBytes(status.counters.bytes_received)}
                  </p>
                )}
              </div>
            );
          })}
        </div>

        {draft && (
          <AsidePanelSection title={draft.id === null ? "新建转发" : "修改转发"}>
            <div className="space-y-3">
              {hostId === null && (
                <div className="space-y-1.5">
                  <Label>主机</Label>
                  <Select
                    value={String(draft.host_id)}
                    onValueChange={(value) => set("host_id", Number(value))}
                  >
                    <SelectTrigger className="h-9">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {hosts.map((host) => (
                        <SelectItem key={host.id} value={String(host.id)}>
                          {host.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              )}
              <div className="space-y-1.5">
                <Label>名称</Label>
                <Input
                  value={draft.name}
                  placeholder="比如：数据库"
                  onChange={(event) => set("name", event.target.value)}
                  className="h-9"
                />
              </div>
              <div className="space-y-1.5">
                <Label>方向</Label>
                <Select value={draft.kind} onValueChange={(value) => set("kind", value as ForwardKind)}>
                  <SelectTrigger className="h-9">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="local">本地转发（-L）：本机端口 → 服务器那边</SelectItem>
                    <SelectItem value="remote">远程转发（-R）：服务器端口 → 本机这边</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="grid grid-cols-[1fr_96px] gap-2">
                <div className="space-y-1.5">
                  <Label>{draft.kind === "local" ? "本机监听地址" : "服务器监听地址"}</Label>
                  <Input
                    value={draft.bind_host}
                    onChange={(event) => set("bind_host", event.target.value)}
                    className="h-9 font-mono text-xs"
                    placeholder="127.0.0.1"
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>端口</Label>
                  <Input
                    type="number"
                    min={1}
                    max={65535}
                    value={draft.bind_port}
                    onChange={(event) => set("bind_port", Number(event.target.value))}
                    className="h-9 font-mono text-xs"
                  />
                </div>
              </div>
              <div className="grid grid-cols-[1fr_96px] gap-2">
                <div className="space-y-1.5">
                  <Label>{draft.kind === "local" ? "目标地址（从服务器看）" : "目标地址（从本机看）"}</Label>
                  <Input
                    value={draft.dest_host}
                    onChange={(event) => set("dest_host", event.target.value)}
                    className="h-9 font-mono text-xs"
                    placeholder="localhost"
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>端口</Label>
                  <Input
                    type="number"
                    min={1}
                    max={65535}
                    value={draft.dest_port}
                    onChange={(event) => set("dest_port", Number(event.target.value))}
                    className="h-9 font-mono text-xs"
                  />
                </div>
              </div>
              {draft.bind_host.trim() !== "" &&
                !["127.0.0.1", "localhost", "::1", "[::1]"].includes(draft.bind_host.trim()) && (
                  <p className="text-xs text-amber-500">
                    监听的不是回环地址：同一网络里的其他机器也能连上这个端口。
                  </p>
                )}
              <div className="flex items-center justify-between gap-2">
                <div>
                  <Label>连上这台主机时自动开启</Label>
                  <p className="text-xs text-muted-foreground">打开它的终端标签时一起开。</p>
                </div>
                <Switch
                  checked={draft.auto_start}
                  onCheckedChange={(checked) => set("auto_start", checked)}
                />
              </div>
            </div>
          </AsidePanelSection>
        )}
      </AsidePanelContent>

      {draft && (
        <AsidePanelFooter className="flex items-center gap-2">
          {draft.id !== null && (
            <Button
              variant="ghost"
              size="sm"
              className="mr-auto text-destructive hover:bg-destructive/10"
              disabled={busy}
              onClick={() => void remove()}
            >
              删除
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={() => setDraft(null)} disabled={busy}>
            取消
          </Button>
          <Button size="sm" onClick={() => void save()} disabled={busy}>
            保存
          </Button>
        </AsidePanelFooter>
      )}
    </AsidePanel>
  );
}
