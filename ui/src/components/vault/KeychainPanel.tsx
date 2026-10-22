//! 凭据管理面板（参照 Netcatty components/KeychainManager.tsx 的样式：
//! 列表 + 选中后展开编辑）。数据走我们自己的 stores/vault。

import { useState } from "react";

import { identityPublicKey, type Identity, type IdentityKind } from "../../api/vault";
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
import { Textarea } from "../ui/textarea";
import {
  identityKindChangeClearsSecret,
  identitySecretToSend,
  inlinePassphraseToSend,
} from "./authIdentity";
import { GenerateKeyDialog } from "./SshKeyDialogs";

const IDENTITY_KINDS: { value: IdentityKind; label: string }[] = [
  { value: "password", label: "密码" },
  { value: "key_file", label: "私钥文件" },
  { value: "key_inline", label: "导入的私钥" },
  { value: "agent", label: "ssh-agent" },
];

const KIND_LABELS: Record<IdentityKind, string> = {
  password: "密码",
  key_file: "私钥文件",
  key_inline: "导入的私钥",
  agent: "ssh-agent",
};

interface KeychainPanelProps {
  onClose: () => void;
  onError: (message: string) => void;
}

export default function KeychainPanel({ onClose, onError }: KeychainPanelProps) {
  const identities = useVault((state) => state.identities);
  const saveIdentity = useVault((state) => state.saveIdentity);
  const removeIdentity = useVault((state) => state.removeIdentity);

  const [selected, setSelected] = useState<Identity | null>(null);
  const [generating, setGenerating] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<IdentityKind>("password");
  const [keyPath, setKeyPath] = useState("");
  const [secret, setSecret] = useState("");
  // key_inline 用：私钥本身的口令（和私钥内容一起存进密文）
  const [keyPassphrase, setKeyPassphrase] = useState("");
  // key_inline 用：明确要去掉已保存的口令（口令框留空只表示"不改"）
  const [clearPassphrase, setClearPassphrase] = useState(false);
  // 导入私钥的正文（只在 kind = key_inline 且要替换时填）
  const [inlineKey, setInlineKey] = useState("");
  const [busy, setBusy] = useState(false);

  const startNew = () => {
    setSelected(null);
    setName("");
    setKind("password");
    setKeyPath("");
    setSecret("");
    setKeyPassphrase("");
    setClearPassphrase(false);
    setInlineKey("");
  };

  const pick = (identity: Identity) => {
    setSelected(identity);
    setName(identity.name);
    setKind(identity.kind);
    setKeyPath(identity.key_path ?? "");
    setSecret("");
    setKeyPassphrase("");
    setClearPassphrase(false);
    setInlineKey("");
  };

  const submit = async () => {
    if (name.trim() === "") {
      onError("凭据名称不能为空");
      return;
    }
    setBusy(true);
    try {
      await saveIdentity({
        id: selected?.id ?? null,
        name: name.trim(),
        kind,
        key_path: kind === "key_file" ? keyPath.trim() : null,
        // 留空 = 不改动已存的秘密；改了类型时留空 = 清掉；新建时留空就是没有
        secret: identitySecretToSend({
          editing: selected,
          kind,
          secret,
          inlineKey,
        }),
        // 只有导入的私钥用得上：留空 = 不改，勾了清除 = 清掉
        passphrase: inlinePassphraseToSend({
          kind,
          passphrase: keyPassphrase,
          clear: clearPassphrase,
        }),
      });
      startNew();
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!selected) return;
    setBusy(true);
    try {
      await removeIdentity(selected.id);
      startNew();
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const editing = selected !== null || name !== "";
  // 已有导入私钥、类型没改：后端会沿用原口令，"留空不改"和"清除口令"才有意义
  const canKeepPassphrase =
    selected !== null &&
    selected.kind === "key_inline" &&
    selected.has_secret &&
    kind === "key_inline";

  return (
    <AsidePanel
      title="凭据"
      subtitle={`${identities.length} 条`}
      onClose={onClose}
      actions={
        <>
          <Button variant="ghost" size="sm" onClick={() => setGenerating(true)}>
            生成密钥
          </Button>
          <Button variant="ghost" size="sm" onClick={startNew}>
            新建
          </Button>
        </>
      }
    >
      <AsidePanelContent>
        <div className="space-y-1">
          {identities.length === 0 && (
            <p className="px-1 py-2 text-sm text-muted-foreground">
              还没有凭据。新建一条，或者在主机详情里填密码时勾选"存成凭据"。
            </p>
          )}
          {identities.map((identity) => {
            const isSelected = selected?.id === identity.id;
            return (
              <button
                key={identity.id}
                onClick={() => pick(identity)}
                className={cn(
                  "flex w-full cursor-pointer items-center gap-2 rounded-md border px-3 py-2 text-left transition-colors",
                  isSelected
                    ? "border-primary bg-primary text-primary-foreground"
                    : "border-border/60 hover:bg-accent/40",
                )}
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm">{identity.name}</span>
                  <span
                    className={cn(
                      "block truncate text-xs",
                      isSelected ? "text-primary-foreground/70" : "text-muted-foreground",
                    )}
                  >
                    {KIND_LABELS[identity.kind]}
                    {identity.key_path ? ` · ${identity.key_path}` : ""}
                  </span>
                </span>
                <Badge
                  variant={identity.has_secret ? "secondary" : "outline"}
                  className={
                    isSelected
                      ? "border-transparent bg-primary-foreground/15 text-primary-foreground"
                      : undefined
                  }
                >
                  {identity.has_secret ? "已保存" : "无密码"}
                </Badge>
              </button>
            );
          })}
        </div>

        {editing && (
          <AsidePanelSection title={selected ? "编辑凭据" : "新建凭据"}>
            <div className="space-y-1.5">
              <Label>名称</Label>
              <Input value={name} onChange={(event) => setName(event.target.value)} className="h-9" />
            </div>
            <div className="space-y-1.5">
              <Label>类型</Label>
              <Select value={kind} onValueChange={(value) => setKind(value as IdentityKind)}>
                <SelectTrigger className="h-9">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {IDENTITY_KINDS.map((item) => (
                    <SelectItem key={item.value} value={item.value}>
                      {item.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {selected && identityKindChangeClearsSecret(selected, kind) && (
                <p className="text-xs text-muted-foreground">
                  改了类型：原来保存的{KIND_LABELS[selected.kind]}
                  内容不会带过来，保存时会清掉；需要的话在下面重新填写。
                </p>
              )}
            </div>
            {kind === "key_file" && (
              <div className="space-y-1.5">
                <Label>私钥文件路径</Label>
                <Input
                  value={keyPath}
                  onChange={(event) => setKeyPath(event.target.value)}
                  className="h-9 font-mono text-xs"
                  placeholder="C:\\Users\\me\\.ssh\\id_ed25519"
                  spellCheck={false}
                />
              </div>
            )}
            {kind === "key_inline" && (
              <>
                <div className="space-y-1.5">
                  <Label>
                    私钥内容
                    <span className="ml-2 text-xs font-normal text-muted-foreground">
                      {selected?.has_secret ? "留空表示不改" : "粘贴私钥文本"}
                    </span>
                  </Label>
                  <Textarea
                    value={inlineKey}
                    onChange={(event) => setInlineKey(event.target.value)}
                    className="min-h-[96px] font-mono text-xs"
                    placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                    spellCheck={false}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>
                    私钥口令
                    <span className="ml-2 text-xs font-normal text-muted-foreground">
                      {canKeepPassphrase ? "留空表示不改" : "私钥没有口令就留空"}
                    </span>
                  </Label>
                  <Input
                    type="password"
                    value={keyPassphrase}
                    onChange={(event) => setKeyPassphrase(event.target.value)}
                    className="h-9"
                    autoComplete="off"
                    disabled={clearPassphrase}
                  />
                  {canKeepPassphrase && (
                    <label className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Switch checked={clearPassphrase} onCheckedChange={setClearPassphrase} />
                      清除已保存的口令（私钥没有口令时用）
                    </label>
                  )}
                </div>
              </>
            )}
            {(kind === "password" || kind === "key_file") && (
              <div className="space-y-1.5">
                <Label>
                  {kind === "password" ? "密码" : "私钥口令"}
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
            )}
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
              onClick={() => void remove()}
            >
              删除
            </Button>
          )}
          {selected && (selected.kind === "key_inline" || selected.kind === "key_file") && (
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => {
                void identityPublicKey(selected.id)
                  .then((view) => navigator.clipboard?.writeText(view.public_key))
                  .catch((error: unknown) =>
                    onError(`读取公钥失败：${error instanceof Error ? error.message : String(error)}`),
                  );
              }}
            >
              复制公钥
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={startNew} disabled={busy}>
            清空
          </Button>
          <Button size="sm" onClick={() => void submit()} disabled={busy}>
            保存
          </Button>
        </AsidePanelFooter>
      )}
      {generating && (
        <GenerateKeyDialog
          onClose={() => setGenerating(false)}
          onGenerated={(identity) => pick(identity)}
          onError={onError}
        />
      )}
    </AsidePanel>
  );
}
