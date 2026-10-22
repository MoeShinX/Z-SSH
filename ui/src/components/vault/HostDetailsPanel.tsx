//! 主机编辑面板（参照 Netcatty components/HostDetailsPanel.tsx：从右侧滑出、
//! 分组字段、高级区折叠、指纹管理）。
//!
//! 字段和 M1-03～M1-05 一致；数据走我们自己的 stores/vault。

import { useEffect, useState } from "react";
import { Network } from "lucide-react";

import { hostkeyForget, hostkeySaved } from "../../api/auth";
import type { AuthMethod, Host, HostDraft, ProxyMode } from "../../api/vault";
import { cn } from "../../lib/utils";
import { identityFitsAuth } from "./authIdentity";
import HostMemorySection from "./HostMemorySection";
import ProxyPanel from "./ProxyPanel";
import { useVault } from "../../stores/vault";
import {
  AsidePanel,
  AsidePanelContent,
  AsidePanelFooter,
  AsidePanelSection,
} from "../ui/aside-panel";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { normalizeHostInput } from "../../state/hostAddress";

const AUTH_LABELS: Record<AuthMethod, string> = {
  password: "密码",
  key: "私钥",
  agent: "ssh-agent",
};

interface HostDetailsPanelProps {
  /** null 表示新建 */
  host: Host | null;
  /** 新建时默认落在哪个分组 */
  groupId: number | null;
  onClose: () => void;
  onSaved: (host: Host) => void;
  onDelete: (host: Host) => void;
  onError: (message: string) => void;
}

export default function HostDetailsPanel({
  host,
  groupId,
  onClose,
  onSaved,
  onDelete,
  onError,
}: HostDetailsPanelProps) {
  const groups = useVault((state) => state.groups);
  const hosts = useVault((state) => state.hosts);
  const identities = useVault((state) => state.identities);
  const proxies = useVault((state) => state.proxies);
  const saveHost = useVault((state) => state.saveHost);
  const saveIdentity = useVault((state) => state.saveIdentity);

  const [name, setName] = useState(host?.name ?? "");
  const [group, setGroup] = useState(
    host
      ? host.group_id === null
        ? "none"
        : String(host.group_id)
      : groupId === null
        ? "none"
        : String(groupId),
  );
  const [address, setAddress] = useState(host?.host ?? "");
  const [port, setPort] = useState(String(host?.port ?? 22));
  // 新建时默认 root（和端口默认 22 一样，可以直接改）；编辑已有主机显示它保存的用户名。
  const [username, setUsername] = useState(host ? host.username : "root");
  const [authMethod, setAuthMethod] = useState<AuthMethod>(host?.auth_method ?? "password");
  const [identityId, setIdentityId] = useState(host?.identity_id == null ? "none" : String(host.identity_id));
  // 新建主机默认不使用代理（不跟随全局默认代理）；编辑已有主机显示它保存的值。
  const [proxyMode, setProxyMode] = useState<ProxyMode>(host?.proxy_mode ?? "none");
  const [proxyId, setProxyId] = useState(host?.proxy_id == null ? "none" : String(host.proxy_id));
  const [jumpHostId, setJumpHostId] = useState(host?.jump_host_id == null ? "none" : String(host.jump_host_id));
  // 留空表示不改已存的密码
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);
  const [legacyAlgorithms, setLegacyAlgorithms] = useState(host?.legacy_algorithms ?? false);
  const [savedFingerprint, setSavedFingerprint] = useState<string | null>(null);
  // 「新建 / 管理代理」打开的代理面板（R-11）。由这个组件自己渲染、盖在主机详情的
  // 位置上：上面这些编辑中的字段都还在本组件的 state 里，回来时一个不丢；
  // 新建出来的代理也能直接选中，不用用户再去下拉框里找。
  const [managingProxies, setManagingProxies] = useState(false);

  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    void hostkeySaved(host.host, host.port)
      .then((fingerprint) => {
        if (!cancelled) setSavedFingerprint(fingerprint);
      })
      .catch((error: unknown) => {
        console.error("读已保存指纹失败", error);
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  const selectedIdentity = identities.find((item) => String(item.id) === identityId);
  // 只列和认证方式配套的凭据（R-10 H2）：密码认证选了私钥凭据，私钥会被当成密码发给服务器。
  const fittingIdentities = identities.filter((item) => identityFitsAuth(item.kind, authMethod));
  // 原来存的组合就不配套（旧数据或同步来的）：取消选择，并告诉用户为什么。
  const [mismatchNote, setMismatchNote] = useState<string | null>(null);
  useEffect(() => {
    if (selectedIdentity && !identityFitsAuth(selectedIdentity.kind, authMethod)) {
      setMismatchNote(`凭据「${selectedIdentity.name}」和认证方式不配套，已取消选择`);
      setIdentityId("none");
    }
  }, [selectedIdentity, authMethod]);

  const submit = async () => {
    const portNumber = Number(port);
    if (name.trim() === "" || address.trim() === "" || username.trim() === "") {
      onError("名称、地址、用户名都不能为空");
      return;
    }
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) {
      onError("端口要在 1-65535 之间");
      return;
    }

    setBusy(true);
    try {
      let resolvedIdentity: number | null = identityId === "none" ? null : Number(identityId);

      // 填了密码才动保险库：选了凭据就覆盖它的密码，没选就新建一条
      if (password !== "" && authMethod === "password") {
        if (resolvedIdentity !== null) {
          const selected = identities.find((item) => item.id === resolvedIdentity);
          await saveIdentity({
            id: resolvedIdentity,
            name: selected?.name ?? `${name.trim()} 的密码`,
            kind: "password",
            key_path: null,
            secret: password,
          });
        } else if (remember) {
          const created = await saveIdentity({
            id: null,
            name: `${name.trim()} 的密码`,
            kind: "password",
            key_path: null,
            secret: password,
          });
          resolvedIdentity = created.id;
        }
      }

      const draft: HostDraft = {
        // 注意：不能关掉密码保存后把 identity_id 也清掉，那就等于吊销凭据了
        id: host?.id ?? null,
        group_id: group === "none" ? null : Number(group),
        name: name.trim(),
        host: normalizeHostInput(address),
        port: portNumber,
        username: username.trim(),
        auth_method: authMethod,
        identity_id: resolvedIdentity,
        proxy_mode: proxyMode,
        proxy_id: proxyId === "none" ? null : Number(proxyId),
        jump_host_id: jumpHostId === "none" ? null : Number(jumpHostId),
        // 标签这张卡不在界面上编辑，改的时候保持原样
        tags: host?.tags ?? [],
        sort: host?.sort ?? 0,
        legacy_algorithms: legacyAlgorithms,
      };
      const saved = await saveHost(draft);
      onSaved(saved);
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const passwordHint =
    host && selectedIdentity?.has_secret
      ? "留空表示不改保险库里的密码"
      : "留空表示不保存密码，连接时会提示";

  if (managingProxies) {
    return (
      <ProxyPanel
        startCreating={proxies.length === 0}
        onBack={() => setManagingProxies(false)}
        onClose={() => setManagingProxies(false)}
        onError={onError}
        onSaved={(saved, created) => {
          if (!created) return;
          setProxyMode("custom");
          setProxyId(String(saved.id));
          setManagingProxies(false);
        }}
      />
    );
  }

  return (
    <AsidePanel
      title={host ? "主机详情" : "新建主机"}
      subtitle={host ? `${host.username}@${host.host}:${host.port}` : "填好之后保存到主机库"}
      onClose={onClose}
    >
      <AsidePanelContent>
        <AsidePanelSection title="基本">
          <div className="space-y-1.5">
            <Label>名称</Label>
            <Input
              value={name}
              onChange={(event) => setName(event.target.value)}
              className="h-9"
              autoFocus
            />
          </div>
          <div className="space-y-1.5">
            <Label>分组</Label>
            <Select value={group} onValueChange={setGroup}>
              <SelectTrigger className="h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">未分组</SelectItem>
                {groups.map((item) => (
                  <SelectItem key={item.id} value={String(item.id)}>
                    {item.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </AsidePanelSection>

        <AsidePanelSection title="连接">
          <div className="flex gap-2">
            <div className="min-w-0 flex-1 space-y-1.5">
              <Label>地址</Label>
              <Input
                value={address}
                onChange={(event) => setAddress(event.target.value)}
                className="h-9 font-mono"
                placeholder="10.0.0.11"
                spellCheck={false}
              />
            </div>
            <div className="w-20 shrink-0 space-y-1.5">
              <Label>端口</Label>
              <Input
                value={port}
                onChange={(event) => setPort(event.target.value)}
                className="h-9"
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <Label>用户名</Label>
            <Input
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              className="h-9"
              spellCheck={false}
            />
          </div>
          <div className="space-y-1.5">
            <Label>
              密码
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                {passwordHint}
              </span>
            </Label>
            <Input
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              className="h-9"
              autoComplete="off"
            />
          </div>
          <label className="flex items-center justify-between gap-2 text-[13px]">
            <span>没有选凭据时，把密码存成一条新凭据</span>
            <Switch checked={remember} onCheckedChange={setRemember} />
          </label>
        </AsidePanelSection>

        <AsidePanelSection title="认证">
          <div className="space-y-1.5">
            <Label>认证方式</Label>
            <Select value={authMethod} onValueChange={(value) => setAuthMethod(value as AuthMethod)}>
              <SelectTrigger className="h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(Object.keys(AUTH_LABELS) as AuthMethod[]).map((method) => (
                  <SelectItem key={method} value={method}>
                    {AUTH_LABELS[method]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label>保存的凭据</Label>
            <Select
              value={identityId}
              onValueChange={(value) => {
                setIdentityId(value);
                setMismatchNote(null);
              }}
            >
              <SelectTrigger className="h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">不用</SelectItem>
                {fittingIdentities.map((item) => (
                  <SelectItem key={item.id} value={String(item.id)}>
                    {item.name}
                    {item.has_secret ? "" : "（没有密码）"}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {mismatchNote && <p className="text-xs text-amber-600">{mismatchNote}</p>}
          </div>
        </AsidePanelSection>

        <AsidePanelSection title="网络">
          <div className="space-y-1.5">
            <Label>代理</Label>
            <Select value={proxyMode} onValueChange={(value) => setProxyMode(value as ProxyMode)}>
              <SelectTrigger className="h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="default">跟随默认代理</SelectItem>
                <SelectItem value="none">不用代理</SelectItem>
                <SelectItem value="custom">指定代理</SelectItem>
              </SelectContent>
            </Select>
          </div>
          {proxyMode === "custom" && (
            <div className="space-y-1.5">
              <Label>用哪个代理</Label>
              <Select value={proxyId} onValueChange={setProxyId}>
                <SelectTrigger className="h-9">
                  <SelectValue placeholder="（请选择）" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">（请选择）</SelectItem>
                  {proxies.map((item) => (
                    <SelectItem key={item.id} value={String(item.id)}>
                      {item.name}（{item.kind}）
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {proxies.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  还没有代理，点下面的「新建 / 管理代理」添加一个。
                </p>
              )}
              <Button
                variant="outline"
                size="sm"
                className="h-8 gap-1.5 self-start text-xs"
                onClick={() => setManagingProxies(true)}
              >
                <Network size={13} />
                新建 / 管理代理
              </Button>
            </div>
          )}
          <div className="space-y-1.5">
            <Label>跳板机</Label>
            <Select value={jumpHostId} onValueChange={setJumpHostId}>
              <SelectTrigger className="h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">不使用</SelectItem>
                {hosts
                  .filter((item) => item.id !== host?.id)
                  .map((item) => (
                    <SelectItem key={item.id} value={String(item.id)}>
                      {item.name}
                    </SelectItem>
                  ))}
              </SelectContent>
            </Select>
          </div>
        </AsidePanelSection>

        <AsidePanelSection title="高级">
          <div className="space-y-3 rounded-md border border-border/60 p-3">
            <label className="flex items-start justify-between gap-3">
              <span className="text-[13px]">
                旧算法兼容
                <span className="mt-1 block text-xs text-amber-500 dark:text-amber-400">
                  仅用于老服务器：允许 SHA1 签名的 ssh-rsa、老的密钥交换和 CBC 加密。
                  安全性较低，只在这台机器连不上、并且确认对方是老系统时才打开。
                </span>
              </span>
              <Switch checked={legacyAlgorithms} onCheckedChange={setLegacyAlgorithms} />
            </label>
          </div>
          <div className="space-y-2 rounded-md border border-border/60 p-3">
            <span className="text-xs text-muted-foreground">已保存的主机指纹</span>
            {host ? (
              <>
                <p
                  className={cn(
                    "break-all font-mono text-[11px]",
                    savedFingerprint ? "text-foreground" : "text-muted-foreground",
                  )}
                >
                  {savedFingerprint ?? "（没有保存，下次连接会问）"}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    void hostkeyForget(host.host, host.port)
                      .then((removed) => {
                        setSavedFingerprint(null);
                        onError(
                          removed > 0
                            ? `已清除 ${removed} 条指纹记录，下次连接会重新询问`
                            : "本来就没有保存过这台主机的指纹",
                        );
                      })
                      .catch((error: unknown) => onError(String(error)));
                  }}
                >
                  清除已保存的指纹
                </Button>
                <p className="text-xs text-muted-foreground">
                  服务器换过密钥（比如重装系统）时用；如果是被人冒充，先查清楚原因再清。
                </p>
              </>
            ) : (
              <p className="text-xs text-muted-foreground">保存之后才能看到。</p>
            )}
          </div>
        </AsidePanelSection>

        {host && <HostMemorySection hostId={host.id} onError={onError} />}
      </AsidePanelContent>

      <AsidePanelFooter className="flex items-center gap-2">
        {host && (
          <Button
            variant="ghost"
            size="sm"
            className="mr-auto text-destructive hover:bg-destructive/10"
            onClick={() => onDelete(host)}
          >
            删除
          </Button>
        )}
        <Button variant="outline" size="sm" onClick={onClose}>
          取消
        </Button>
        <Button size="sm" onClick={() => void submit()} disabled={busy}>
          保存
        </Button>
      </AsidePanelFooter>
    </AsidePanel>
  );
}
