//! 代理管理面板（参照 Netcatty components/ProxyProfilesManager.tsx 的样式）。
//! 顶部是全局默认代理的选择，下面是可以增删改的代理列表。

import { useEffect, useState } from "react";
import { Pencil, Plus, Trash2 } from "lucide-react";

import type { Proxy, ProxyKind } from "../../api/vault";
import { defaultProxyGet, defaultProxySet } from "../../api/vault";
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
import { ConfirmDialog } from "../ui/confirm-dialog";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";

const PROXY_KINDS: { value: ProxyKind; label: string }[] = [
  { value: "socks5", label: "SOCKS5" },
  { value: "http", label: "HTTP CONNECT" },
];

interface ProxyPanelProps {
  onClose: () => void;
  onError: (message: string) => void;
  /** 一打开就是"新建代理"的表单（从主机详情点「新建 / 管理代理」进来时）。 */
  startCreating?: boolean;
  /** 保存成功；`created` 表示这是新建的（主机详情拿它自动选中）。 */
  onSaved?: (proxy: Proxy, created: boolean) => void;
  /** 标题栏的返回键（从主机详情进来时回主机详情）。 */
  onBack?: () => void;
}

export default function ProxyPanel({
  onClose,
  onError,
  startCreating = false,
  onSaved,
  onBack,
}: ProxyPanelProps) {
  const proxies = useVault((state) => state.proxies);
  const saveProxy = useVault((state) => state.saveProxy);
  const removeProxy = useVault((state) => state.removeProxy);

  const [selected, setSelected] = useState<Proxy | null>(null);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<ProxyKind>("socks5");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("10808");
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  // 正在填新建表单。以前用 `name !== ""` 判断要不要显示表单，点「新建」清空名字后
  // 表单反而藏起来，用户根本没地方填（R-11）。
  const [creating, setCreating] = useState(startCreating);

  // 全局默认代理：快速连接和"跟随默认代理"的主机都用它
  const [defaultProxyId, setDefaultProxyId] = useState<string>("none");
  // 等用户确认删除的代理
  const [confirmDelete, setConfirmDelete] = useState<Proxy | null>(null);

  const loadDefault = () =>
    defaultProxyGet().then((id) => (id === null ? "none" : String(id)));

  useEffect(() => {
    let cancelled = false;
    void loadDefault()
      .then((value) => {
        if (!cancelled) setDefaultProxyId(value);
      })
      .catch((error: unknown) => {
        console.error("读默认代理失败", error);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const resetForm = () => {
    setSelected(null);
    setName("");
    setKind("socks5");
    setHost("");
    setPort("10808");
    setUsername("");
    setSecret("");
  };

  const startNew = () => {
    resetForm();
    setCreating(true);
  };

  /** 收起表单，回到只看列表。 */
  const closeForm = () => {
    resetForm();
    setCreating(false);
  };

  const pick = (proxy: Proxy) => {
    setCreating(false);
    setSelected(proxy);
    setName(proxy.name);
    setKind(proxy.kind);
    setHost(proxy.host);
    setPort(String(proxy.port));
    setUsername(proxy.username ?? "");
    setSecret("");
  };

  const submit = async () => {
    const portNumber = Number(port);
    if (name.trim() === "" || host.trim() === "") {
      onError("代理名称和地址不能为空");
      return;
    }
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) {
      onError("端口要在 1-65535 之间");
      return;
    }
    setBusy(true);
    try {
      const created = selected === null;
      const saved = await saveProxy({
        id: selected?.id ?? null,
        name: name.trim(),
        kind,
        host: host.trim(),
        port: portNumber,
        username: username.trim() === "" ? null : username.trim(),
        // 留空 = 不改动已存的密码
        secret: secret === "" ? null : secret,
      });
      closeForm();
      onSaved?.(saved, created);
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (proxy: Proxy) => {
    setBusy(true);
    try {
      await removeProxy(proxy.id);
      if (selected?.id === proxy.id) closeForm();
      // 删的是默认代理时后端已经把默认改成"不用"，这里跟着刷新
      setDefaultProxyId(await loadDefault());
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const changeDefault = async (value: string) => {
    setDefaultProxyId(value);
    try {
      await defaultProxySet(value === "none" ? null : Number(value));
    } catch (error: unknown) {
      onError(String(error));
    }
  };

  const editing = selected !== null || creating;

  return (
    <AsidePanel
      title="代理"
      subtitle={`${proxies.length} 个`}
      onClose={onClose}
      showBackButton={onBack !== undefined}
      onBack={onBack}
      actions={
        <Button variant="outline" size="sm" className="h-7 gap-1" onClick={startNew}>
          <Plus size={14} />
          新建代理
        </Button>
      }
    >
      <AsidePanelContent>
        <AsidePanelSection title="默认代理">
          <Select value={defaultProxyId} onValueChange={(value) => void changeDefault(value)}>
            <SelectTrigger className="h-9">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">不使用默认代理</SelectItem>
              {proxies.map((proxy) => (
                <SelectItem key={proxy.id} value={String(proxy.id)}>
                  {proxy.name}（{proxy.kind}）
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            "跟随默认代理"的主机（包括快速连接）都走这一个。
          </p>
          {proxies.length === 0 && (
            <p className="text-xs text-muted-foreground">还没有代理，先点右上角「新建代理」添加一个。</p>
          )}
        </AsidePanelSection>

        <AsidePanelSection title="代理列表">
          <div className="space-y-1">
            {proxies.length === 0 && (
              <p className="px-1 py-2 text-sm text-muted-foreground">
                还没有代理，点右上角「新建代理」添加。
              </p>
            )}
            {proxies.map((proxy) => {
              const isSelected = selected?.id === proxy.id;
              return (
                <div
                  key={proxy.id}
                  className={cn(
                    "group flex w-full items-center gap-1 rounded-md border pr-1 transition-colors",
                    isSelected
                      ? "border-primary bg-primary text-primary-foreground"
                      : "border-border/60 hover:bg-accent/40",
                  )}
                >
                  <button
                    onClick={() => pick(proxy)}
                    className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 px-3 py-2 text-left"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-sm">{proxy.name}</span>
                      <span
                        className={cn(
                          "block truncate font-mono text-xs",
                          isSelected ? "text-primary-foreground/70" : "text-muted-foreground",
                        )}
                      >
                        {proxy.host}:{proxy.port}
                      </span>
                    </span>
                    <Badge
                      variant="secondary"
                      className={
                        isSelected
                          ? "border-transparent bg-primary-foreground/15 text-primary-foreground"
                          : undefined
                      }
                    >
                      {proxy.kind}
                    </Badge>
                  </button>
                  <Button
                    variant="ghost"
                    size="icon"
                    title="编辑"
                    className={cn("h-7 w-7", isSelected && "hover:bg-primary-foreground/15")}
                    onClick={() => pick(proxy)}
                  >
                    <Pencil size={13} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    title="删除"
                    disabled={busy}
                    className={cn(
                      "h-7 w-7",
                      isSelected
                        ? "hover:bg-primary-foreground/15"
                        : "text-muted-foreground hover:bg-destructive/10 hover:text-destructive",
                    )}
                    onClick={() => setConfirmDelete(proxy)}
                  >
                    <Trash2 size={13} />
                  </Button>
                </div>
              );
            })}
          </div>
        </AsidePanelSection>

        {editing && (
          <AsidePanelSection title={selected ? "编辑代理" : "新建代理"}>
            <div className="space-y-1.5">
              <Label>名称</Label>
              <Input value={name} onChange={(event) => setName(event.target.value)} className="h-9" />
            </div>
            <div className="space-y-1.5">
              <Label>类型</Label>
              <Select value={kind} onValueChange={(value) => setKind(value as ProxyKind)}>
                <SelectTrigger className="h-9">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PROXY_KINDS.map((item) => (
                    <SelectItem key={item.value} value={item.value}>
                      {item.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex gap-2">
              <div className="min-w-0 flex-1 space-y-1.5">
                <Label>地址</Label>
                <Input
                  value={host}
                  onChange={(event) => setHost(event.target.value)}
                  className="h-9 font-mono text-xs"
                  placeholder="127.0.0.1"
                  spellCheck={false}
                />
              </div>
              <div className="w-20 shrink-0 space-y-1.5">
                <Label>端口</Label>
                <Input value={port} onChange={(event) => setPort(event.target.value)} className="h-9" />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label>用户名（可选）</Label>
              <Input
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                className="h-9"
                autoComplete="off"
              />
            </div>
            <div className="space-y-1.5">
              <Label>
                密码
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  {selected ? "留空表示不改" : ""}
                </span>
              </Label>
              <Input
                type="password"
                value={secret}
                onChange={(event) => setSecret(event.target.value)}
                className="h-9"
                autoComplete="off"
              />
            </div>
          </AsidePanelSection>
        )}
      </AsidePanelContent>

      {editing && (
        <AsidePanelFooter className="flex items-center gap-2">
          {selected && (
            <Button
              variant="ghost"
              size="sm"
              className="mr-auto text-destructive hover:bg-destructive/10"
              disabled={busy}
              onClick={() => setConfirmDelete(selected)}
            >
              删除
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={closeForm} disabled={busy}>
            取消
          </Button>
          <Button size="sm" onClick={() => void submit()} disabled={busy}>
            保存
          </Button>
        </AsidePanelFooter>
      )}

      {confirmDelete && (
        <ConfirmDialog
          open
          title={`删除代理「${confirmDelete.name}」？`}
          message={
            String(confirmDelete.id) === defaultProxyId
              ? "它是现在的 SSH 默认代理，删除后跟随默认代理的主机改为直连。正在被主机单独使用的代理不能删除。"
              : "正在被主机单独使用的代理不能删除，要先在那些主机里换掉。"
          }
          confirmLabel="删除"
          destructive
          busy={busy}
          onOpenChange={(open) => !open && setConfirmDelete(null)}
          onConfirm={() => {
            const proxy = confirmDelete;
            setConfirmDelete(null);
            void remove(proxy);
          }}
        />
      )}
    </AsidePanel>
  );
}
