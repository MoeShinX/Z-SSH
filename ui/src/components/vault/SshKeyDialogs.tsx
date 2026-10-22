//! SSH 密钥的两个对话框：生成新密钥、把公钥装到服务器上。

import { Check, Copy } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import {
  draftOfHost,
  hostInstallPublicKey,
  identityGenerate,
  identityPublicKey,
  type Host,
  type Identity,
  type KeyAlgorithm,
} from "../../api/vault";
import { useVault } from "../../stores/vault";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { Textarea } from "../ui/textarea";

const ALGORITHMS: { value: KeyAlgorithm; label: string; hint: string }[] = [
  { value: "ed25519", label: "Ed25519（推荐）", hint: "短、快，现在的服务器都支持" },
  { value: "ecdsa_p256", label: "ECDSA P-256", hint: "部分设备或合规要求只认 ECDSA" },
  { value: "rsa4096", label: "RSA 4096", hint: "兼容很老的服务器，生成要几秒" },
];

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 公钥一行 + 复制按钮。 */
export function PublicKeyBox({ publicKey, fingerprint }: { publicKey: string; fingerprint: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="space-y-1.5">
      <Label>公钥</Label>
      <Textarea
        readOnly
        value={publicKey}
        className="min-h-[72px] font-mono text-[11px]"
        onFocus={(event) => event.target.select()}
      />
      <div className="flex items-center justify-between gap-2">
        <span className="truncate font-mono text-[11px] text-muted-foreground" title={fingerprint}>
          {fingerprint}
        </span>
        <Button
          variant="outline"
          size="sm"
          className="h-7 gap-1"
          onClick={() => {
            void navigator.clipboard?.writeText(publicKey).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? <Check size={13} /> : <Copy size={13} />}
          {copied ? "已复制" : "复制公钥"}
        </Button>
      </div>
    </div>
  );
}

/** 生成新密钥：名称、算法、可选口令。生成后显示公钥，可以直接复制。 */
export function GenerateKeyDialog({
  onClose,
  onGenerated,
  onError,
}: {
  onClose: () => void;
  onGenerated?: (identity: Identity) => void;
  onError: (message: string) => void;
}) {
  const load = useVault((state) => state.load);
  const [name, setName] = useState("我的密钥");
  const [algorithm, setAlgorithm] = useState<KeyAlgorithm>("ed25519");
  const [comment, setComment] = useState("z-ssh");
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ publicKey: string; fingerprint: string } | null>(null);

  const generate = async () => {
    if (!name.trim()) {
      onError("名称不能为空");
      return;
    }
    if (passphrase !== confirm) {
      onError("两次输入的口令不一样");
      return;
    }
    setBusy(true);
    try {
      const identity = await identityGenerate({
        name: name.trim(),
        algorithm,
        comment,
        passphrase: passphrase || null,
      });
      await load();
      const view = await identityPublicKey(identity.id);
      setResult({ publicKey: view.public_key, fingerprint: view.fingerprint });
      onGenerated?.(identity);
    } catch (error) {
      onError(`生成失败：${errorText(error)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-[460px]">
        <DialogHeader>
          <DialogTitle>{result ? "密钥已生成" : "生成 SSH 密钥"}</DialogTitle>
          <DialogDescription>
            {result
              ? "私钥已经加密存进凭据库。把下面的公钥装到服务器上就能用这把密钥登录（主机右键「安装公钥」可以一键装）。"
              : "私钥存在本机凭据库里（Windows 数据保护加密），不会离开这台电脑，除非开了配置同步。"}
          </DialogDescription>
        </DialogHeader>
        {result ? (
          <PublicKeyBox publicKey={result.publicKey} fingerprint={result.fingerprint} />
        ) : (
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label>名称</Label>
              <Input value={name} onChange={(event) => setName(event.target.value)} className="h-9" />
            </div>
            <div className="space-y-1.5">
              <Label>算法</Label>
              <Select value={algorithm} onValueChange={(value) => setAlgorithm(value as KeyAlgorithm)}>
                <SelectTrigger className="h-9">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {ALGORITHMS.map((item) => (
                    <SelectItem key={item.value} value={item.value}>
                      {item.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {ALGORITHMS.find((item) => item.value === algorithm)?.hint}
              </p>
            </div>
            <div className="space-y-1.5">
              <Label>
                注释
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  写在公钥末尾，方便在服务器上认出是哪把
                </span>
              </Label>
              <Input
                value={comment}
                onChange={(event) => setComment(event.target.value)}
                className="h-9 font-mono text-xs"
                placeholder="me@laptop"
              />
            </div>
            <div className="grid grid-cols-2 gap-2">
              <div className="space-y-1.5">
                <Label>
                  口令
                  <span className="ml-1 text-xs font-normal text-muted-foreground">可选</span>
                </Label>
                <Input
                  type="password"
                  value={passphrase}
                  onChange={(event) => setPassphrase(event.target.value)}
                  className="h-9"
                  autoComplete="new-password"
                />
              </div>
              <div className="space-y-1.5">
                <Label>再输一次</Label>
                <Input
                  type="password"
                  value={confirm}
                  onChange={(event) => setConfirm(event.target.value)}
                  className="h-9"
                  autoComplete="new-password"
                />
              </div>
            </div>
          </div>
        )}
        <DialogFooter>
          {result ? (
            <Button onClick={onClose}>完成</Button>
          ) : (
            <>
              <Button variant="outline" onClick={onClose} disabled={busy}>
                取消
              </Button>
              <Button onClick={() => void generate()} disabled={busy}>
                {busy ? "生成中…" : "生成"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 把一个私钥凭据的公钥装到主机上；可以顺手把主机改成用这把密钥登录。 */
export function InstallKeyDialog({
  host,
  onClose,
  onDone,
  onError,
}: {
  host: Host;
  onClose: () => void;
  onDone: (message: string) => void;
  onError: (message: string) => void;
}) {
  const identities = useVault((state) => state.identities);
  const saveHost = useVault((state) => state.saveHost);
  const keys = useMemo(
    () => identities.filter((item) => item.kind === "key_inline" || item.kind === "key_file"),
    [identities],
  );
  const [identityId, setIdentityId] = useState<string>(keys[0] ? String(keys[0].id) : "");
  const [switchAuth, setSwitchAuth] = useState(true);
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<{ publicKey: string; fingerprint: string } | null>(null);
  const [generating, setGenerating] = useState(false);

  useEffect(() => {
    if (!identityId) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    void identityPublicKey(Number(identityId))
      .then((view) => {
        if (!cancelled) setPreview({ publicKey: view.public_key, fingerprint: view.fingerprint });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setPreview(null);
          onError(errorText(error));
        }
      });
    return () => {
      cancelled = true;
    };
    // onError 每次渲染都是新函数，放进依赖会反复请求公钥
  }, [identityId]);

  const install = async () => {
    if (!identityId) return;
    setBusy(true);
    try {
      const outcome = await hostInstallPublicKey(host.id, Number(identityId));
      if (switchAuth) {
        await saveHost({ ...draftOfHost(host), auth_method: "key", identity_id: Number(identityId) });
      }
      onDone(
        `${outcome === "added" ? "公钥已装到" : "公钥本来就在"}「${host.name}」上${
          switchAuth ? "，这台主机改用密钥登录" : ""
        }`,
      );
    } catch (error) {
      onError(`安装公钥失败：${errorText(error)}`);
    } finally {
      setBusy(false);
    }
  };

  if (generating) {
    return (
      <GenerateKeyDialog
        onClose={() => setGenerating(false)}
        onGenerated={(identity) => setIdentityId(String(identity.id))}
        onError={onError}
      />
    );
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-[460px]">
        <DialogHeader>
          <DialogTitle>把公钥装到「{host.name}」</DialogTitle>
          <DialogDescription>
            用这台主机现在的登录方式连上，把公钥追加到 ~/.ssh/authorized_keys（已经有就不重复加）。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div className="space-y-1.5">
            <Label>用哪把密钥</Label>
            {keys.length === 0 ? (
              <p className="text-sm text-muted-foreground">凭据库里还没有私钥。</p>
            ) : (
              <Select value={identityId} onValueChange={setIdentityId}>
                <SelectTrigger className="h-9">
                  <SelectValue placeholder="选择一把密钥" />
                </SelectTrigger>
                <SelectContent>
                  {keys.map((item) => (
                    <SelectItem key={item.id} value={String(item.id)}>
                      {item.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            <Button
              variant="outline"
              size="sm"
              className="h-7"
              onClick={() => setGenerating(true)}
              disabled={busy}
            >
              生成一把新密钥…
            </Button>
          </div>
          {preview && <PublicKeyBox publicKey={preview.publicKey} fingerprint={preview.fingerprint} />}
          <div className="flex items-center justify-between gap-2">
            <div>
              <Label>装好后这台主机改用密钥登录</Label>
              <p className="text-xs text-muted-foreground">认证方式改成私钥，凭据选这把。</p>
            </div>
            <Switch checked={switchAuth} onCheckedChange={setSwitchAuth} />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button onClick={() => void install()} disabled={busy || !identityId}>
            {busy ? "安装中…" : "安装"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
