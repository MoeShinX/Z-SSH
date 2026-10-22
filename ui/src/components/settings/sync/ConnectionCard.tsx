// Adapted from Netcatty components/cloud-sync/CloudSyncDialogs.tsx (WebDAV / S3 connection forms)
//! 连接存储：选 WebDAV 或 S3、填连接信息、保存并测试连接（读 / 写 / 并发保护分别显示）。
//! 失败只在这张卡片里显示，不弹窗。

import { Loader2 } from "lucide-react";
import { useEffect, useState } from "react";

import {
  syncGithubCreateGist,
  syncOnedriveDisconnect,
  syncOnedriveLogin,
  syncOnedriveLoginCancel,
  syncTestConnection,
  type ConfigInput,
  type Provider,
  type SyncStatusView,
} from "../../../api/sync";
import { concurrencyNotice } from "../../../state/syncModel";
import { useSync } from "../../../stores/sync";
import { cn } from "../../../lib/utils";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../../ui/select";
import { SectionCard } from "../settingsUi";
import GithubFields, { type GithubFormValue } from "./GithubFields";
import { OnedriveLoginPanel, OnedriveSummary } from "./OnedrivePanel";

interface FormState {
  provider: Provider;
  url: string;
  directory: string;
  username: string;
  password: string;
  allowHttp: boolean;
  endpoint: string;
  region: string;
  bucket: string;
  accessKey: string;
  secretKey: string;
  prefix: string;
  pathStyle: boolean;
  apiBase: string;
  token: string;
  gistId: string;
  /** OneDrive “高级”里自己填的 client ID。 */
  clientId: string;
}

function formFromStatus(status: SyncStatusView | null): FormState {
  const config = status?.config;
  return {
    provider: config?.provider ?? "webdav",
    url: config?.url ?? "",
    directory: config?.directory ?? "Z-SSH",
    username: config?.username ?? "",
    password: "",
    allowHttp: config?.allow_insecure_http ?? false,
    endpoint: config?.endpoint ?? "",
    region: config?.region ?? "us-east-1",
    bucket: config?.bucket ?? "",
    accessKey: config?.access_key ?? "",
    secretKey: "",
    prefix: config?.prefix ?? "z-ssh",
    pathStyle: config?.path_style ?? true,
    // 默认地址留空显示（占位符里写着 https://api.github.com）。
    apiBase:
      config?.provider === "github" && config.api_base !== "https://api.github.com"
        ? config.api_base
        : "",
    token: "",
    gistId: config?.gist_id ?? "",
    clientId: config?.provider === "onedrive" ? config.client_id : "",
  };
}

function toInput(form: FormState): ConfigInput {
  if (form.provider === "webdav") {
    return {
      provider: "webdav",
      url: form.url,
      directory: form.directory,
      username: form.username,
      password: form.password,
      allow_insecure_http: form.allowHttp,
    };
  }
  if (form.provider === "onedrive") {
    // OneDrive 不走“保存表单”，而是浏览器授权登录（syncOnedriveLogin）。
    return { provider: "onedrive" };
  }
  if (form.provider === "github") {
    return {
      provider: "github",
      api_base: form.apiBase,
      token: form.token,
      gist_id: form.gistId,
      allow_insecure_http: form.allowHttp,
    };
  }
  return {
    provider: "s3",
    endpoint: form.endpoint,
    region: form.region,
    bucket: form.bucket,
    access_key: form.accessKey,
    secret_key: form.secretKey,
    prefix: form.prefix,
    path_style: form.pathStyle,
  };
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block space-y-1">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
      {hint && <span className="block text-[11px] text-muted-foreground">{hint}</span>}
    </label>
  );
}

const TONE_CLASS: Record<string, string> = {
  ok: "text-emerald-500",
  warn: "text-amber-500",
  error: "text-red-500",
  idle: "text-muted-foreground",
  busy: "text-muted-foreground",
};

export default function ConnectionCard({ status }: { status: SyncStatusView | null }) {
  const probe = useSync((state) => state.probe);
  const setProbe = useSync((state) => state.setProbe);
  const apply = useSync((state) => state.apply);
  const [form, setForm] = useState<FormState>(() => formFromStatus(status));
  const [editing, setEditing] = useState(!status?.configured);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [loggingIn, setLoggingIn] = useState(false);
  const [disconnecting, setDisconnecting] = useState(false);
  const [justDisconnected, setJustDisconnected] = useState(false);

  const configured = Boolean(status?.configured);
  // 第一次拿到状态（或换了存储）时按已保存的内容填表。
  useEffect(() => {
    setForm((current) => {
      const next = formFromStatus(status);
      // 刚断开 OneDrive：留在 OneDrive 的登录面板上，别跳回默认的 WebDAV。
      if (!status?.configured && current.provider === "onedrive") {
        return { ...next, provider: "onedrive", clientId: current.clientId };
      }
      return next;
    });
    // 只在“是否已配置”变化时重置，避免每次推送状态都冲掉正在输入的内容。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status?.configured]);
  useEffect(() => {
    if (!configured) setEditing(true);
  }, [configured]);

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) =>
    setForm((current) => ({ ...current, [key]: value }));
  // GitHub 表单只认自己的四个字段（它们的类型和 FormState 里同名字段一样）。
  const setGithub = <K extends keyof GithubFormValue>(key: K, value: GithubFormValue[K]) =>
    set(key, value as FormState[K]);

  const plainHttp =
    (form.provider === "webdav" && form.url.trim().toLowerCase().startsWith("http://")) ||
    (form.provider === "github" && form.apiBase.trim().toLowerCase().startsWith("http://"));
  const savedSecret = configured && status?.config?.provider === form.provider && status.config.has_secret;
  const complete =
    form.provider === "github"
      ? (form.token.trim() !== "" || savedSecret) &&
        form.gistId.trim() !== "" &&
        (!plainHttp || form.allowHttp)
      : form.provider === "webdav"
      ? form.url.trim() !== "" &&
        form.username.trim() !== "" &&
        (form.password !== "" || savedSecret) &&
        (!plainHttp || form.allowHttp)
      : form.endpoint.trim() !== "" &&
        form.region.trim() !== "" &&
        form.bucket.trim() !== "" &&
        form.accessKey.trim() !== "" &&
        (form.secretKey !== "" || savedSecret);

  async function saveAndTest() {
    setBusy(true);
    setError(null);
    try {
      const result = await syncTestConnection(toInput(form));
      setProbe(result.probe);
      apply(result.status);
      setForm((current) => ({ ...current, password: "", secretKey: "", token: "" }));
      if (result.probe.read.ok && result.probe.write.ok) setEditing(false);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  /** 新建 secret Gist：成功后把 ID 填进表单，接着自动测试连接。 */
  async function createGist() {
    setCreating(true);
    setError(null);
    try {
      const created = await syncGithubCreateGist(toInput(form));
      apply(created.status);
      const next = { ...form, gistId: created.gist_id };
      setForm(next);
      const result = await syncTestConnection(toInput(next));
      setProbe(result.probe);
      apply(result.status);
      setForm((current) => ({ ...current, token: "" }));
      if (result.probe.read.ok && result.probe.write.ok) setEditing(false);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }

  /** OneDrive 浏览器授权登录：会一直等到登录完成 / 被拒绝 / 超时 / 被取消；成功后自动测试连接。 */
  async function onedriveLogin() {
    setLoggingIn(true);
    setError(null);
    setJustDisconnected(false);
    try {
      apply(await syncOnedriveLogin(form.clientId.trim()));
      setEditing(false);
      try {
        const result = await syncTestConnection();
        setProbe(result.probe);
        apply(result.status);
      } catch (err: unknown) {
        setError(String(err));
      }
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setLoggingIn(false);
    }
  }

  function onedriveCancel() {
    void syncOnedriveLoginCancel().catch(() => {});
  }

  async function onedriveDisconnect() {
    setDisconnecting(true);
    setError(null);
    try {
      apply(await syncOnedriveDisconnect());
      setProbe(null);
      setJustDisconnected(true);
      setEditing(true);
      setForm((current) => ({ ...current, provider: "onedrive" }));
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setDisconnecting(false);
    }
  }

  async function retest() {
    setBusy(true);
    setError(null);
    try {
      const result = await syncTestConnection();
      setProbe(result.probe);
      apply(result.status);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  const notice = status ? concurrencyNotice(status) : null;
  const config = status?.config;

  return (
    <SectionCard
      title="连接存储"
      description="同步包整体加密后存到你自己的 WebDAV、S3 兼容存储、GitHub Gist 或 OneDrive。这里的密码 / Secret / 令牌 / 登录信息只用本机的 Windows 账户加密保存，不会进同步包。"
    >
      {configured && !editing && config && config.provider !== "onedrive" && (
        <div className="space-y-2 py-2 text-[13px]" data-testid="sync-connection-summary">
          <div>
            <span className="text-muted-foreground">类型：</span>
            {config.provider === "webdav"
              ? "WebDAV"
              : config.provider === "github"
              ? "GitHub Gist（secret Gist）"
              : "S3 兼容存储"}
          </div>
          {config.provider === "github" ? (
            <>
              <div className="break-all">
                <span className="text-muted-foreground">接口地址：</span>
                {config.api_base}
              </div>
              <div className="break-all" data-testid="github-summary-gist-id">
                <span className="text-muted-foreground">Gist ID：</span>
                {config.gist_id}
              </div>
              <div className="text-xs text-muted-foreground">
                令牌已保存在本机（加密，不显示）。<strong>第二台电脑上填同一个 Gist ID</strong>
                和一个有 Gist 权限的令牌。secret Gist 拿到链接的人都能打开，但里面是密文。
              </div>
            </>
          ) : config.provider === "webdav" ? (
            <>
              <div className="break-all">
                <span className="text-muted-foreground">地址：</span>
                {config.url}
                {config.directory ? ` / ${config.directory}` : ""}
              </div>
              <div>
                <span className="text-muted-foreground">用户名：</span>
                {config.username}
              </div>
            </>
          ) : (
            <>
              <div className="break-all">
                <span className="text-muted-foreground">Endpoint：</span>
                {config.endpoint}（{config.path_style ? "路径风格" : "虚拟主机风格"}）
              </div>
              <div>
                <span className="text-muted-foreground">Bucket：</span>
                {config.bucket}
                {config.prefix ? ` / ${config.prefix}` : ""}（{config.region}）
              </div>
            </>
          )}
          {config.plain_http && (
            <div className="text-amber-500">
              这个地址是 http://，传输内容没有被加密保护。
              {config.provider === "webdav" ? "WebDAV 的密码会以明文发送。" : ""}
              {config.provider === "github" ? "GitHub 令牌会以明文发送。" : ""}
            </div>
          )}
          <div className="flex gap-2 pt-1">
            <Button size="sm" variant="outline" onClick={() => void retest()} disabled={busy}>
              {busy && <Loader2 size={14} className="animate-spin" />}测试连接
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setEditing(true)} disabled={busy}>
              修改
            </Button>
          </div>
        </div>
      )}

      {status && configured && !editing && config?.provider === "onedrive" && (
        <div className="py-2 text-[13px]" data-testid="sync-connection-summary">
          <OnedriveSummary
            status={status}
            busy={busy || disconnecting}
            loggingIn={loggingIn || status.onedrive_logging_in}
            onTest={() => void retest()}
            onRelogin={() => void onedriveLogin()}
            onCancelLogin={onedriveCancel}
            onDisconnect={() => void onedriveDisconnect()}
          />
        </div>
      )}

      {editing && (
        <div className="space-y-3 py-2" data-testid="sync-connection-form">
          <Field label="存储类型">
            <Select value={form.provider} onValueChange={(v) => set("provider", v as Provider)}>
              <SelectTrigger className="h-9 w-64">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="webdav">WebDAV（Nextcloud、坚果云、Alist、群晖……）</SelectItem>
                <SelectItem value="s3">S3 兼容（AWS、R2、MinIO、OSS、COS……）</SelectItem>
                <SelectItem value="github">GitHub Gist（个人访问令牌）</SelectItem>
                <SelectItem value="onedrive">OneDrive（用微软账号登录）</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          {form.provider === "onedrive" ? (
            <OnedriveLoginPanel
              status={status}
              clientId={form.clientId}
              setClientId={(value) => set("clientId", value)}
              loggingIn={loggingIn || Boolean(status?.onedrive_logging_in)}
              onLogin={() => void onedriveLogin()}
              onCancel={onedriveCancel}
              justDisconnected={justDisconnected}
            />
          ) : form.provider === "github" ? (
            <GithubFields
              form={form}
              set={setGithub}
              savedToken={savedSecret}
              plainHttp={plainHttp}
              creating={creating}
              busy={busy}
              onCreate={() => void createGist()}
            />
          ) : form.provider === "webdav" ? (
            <div className="grid grid-cols-2 gap-3">
              <div className="col-span-2">
                <Field label="WebDAV 地址" hint="例如 https://dav.example.com/remote.php/dav/files/me/">
                  <Input
                    value={form.url}
                    onChange={(e) => set("url", e.target.value)}
                    placeholder="https://"
                    aria-label="WebDAV 地址"
                  />
                </Field>
              </div>
              <Field label="用户名">
                <Input
                  value={form.username}
                  onChange={(e) => set("username", e.target.value)}
                  aria-label="用户名"
                />
              </Field>
              <Field
                label="密码"
                hint={savedSecret ? "留空 = 继续用已保存的密码" : undefined}
              >
                <Input
                  type="password"
                  value={form.password}
                  onChange={(e) => set("password", e.target.value)}
                  aria-label="密码"
                  autoComplete="off"
                />
              </Field>
              <div className="col-span-2">
                <Field label="存放目录（可选）" hint="同步包放在这个子目录里，不存在会自动创建。">
                  <Input
                    value={form.directory}
                    onChange={(e) => set("directory", e.target.value)}
                    aria-label="存放目录"
                  />
                </Field>
              </div>
              {plainHttp && (
                <div
                  className="col-span-2 space-y-2 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-xs"
                  role="alert"
                >
                  <div className="font-medium text-amber-500">地址是 http://，密码会以明文发送</div>
                  <div>
                    同一网络里的人可以看到你的用户名和密码。同步包本身是加密的，但账号密码不是。
                    只在可信的内网里这样用。
                  </div>
                  <label className="flex cursor-pointer items-center gap-2">
                    <input
                      type="checkbox"
                      checked={form.allowHttp}
                      onChange={(e) => set("allowHttp", e.target.checked)}
                      aria-label="我了解密码会以明文发送"
                    />
                    我了解密码会以明文发送，仍然使用 http://
                  </label>
                </div>
              )}
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-3">
              <div className="col-span-2">
                <Field label="Endpoint" hint="例如 https://s3.us-east-1.amazonaws.com、http://127.0.0.1:9000">
                  <Input
                    value={form.endpoint}
                    onChange={(e) => set("endpoint", e.target.value)}
                    placeholder="https://"
                    aria-label="Endpoint"
                  />
                </Field>
              </div>
              <Field label="Region">
                <Input
                  value={form.region}
                  onChange={(e) => set("region", e.target.value)}
                  aria-label="Region"
                />
              </Field>
              <Field label="Bucket">
                <Input
                  value={form.bucket}
                  onChange={(e) => set("bucket", e.target.value)}
                  aria-label="Bucket"
                />
              </Field>
              <Field label="Access Key">
                <Input
                  value={form.accessKey}
                  onChange={(e) => set("accessKey", e.target.value)}
                  aria-label="Access Key"
                  autoComplete="off"
                />
              </Field>
              <Field label="Secret" hint={savedSecret ? "留空 = 继续用已保存的 Secret" : undefined}>
                <Input
                  type="password"
                  value={form.secretKey}
                  onChange={(e) => set("secretKey", e.target.value)}
                  aria-label="Secret"
                  autoComplete="off"
                />
              </Field>
              <Field label="对象路径前缀（可选）">
                <Input
                  value={form.prefix}
                  onChange={(e) => set("prefix", e.target.value)}
                  aria-label="对象路径前缀"
                />
              </Field>
              <Field label="地址风格">
                <Select
                  value={form.pathStyle ? "path" : "virtual"}
                  onValueChange={(v) => set("pathStyle", v === "path")}
                >
                  <SelectTrigger className="h-9">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="path">路径风格（endpoint/bucket/key）</SelectItem>
                    <SelectItem value="virtual">虚拟主机风格（bucket.endpoint/key）</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
            </div>
          )}

          <div className="flex gap-2 pt-1">
            {form.provider !== "onedrive" && (
              <Button size="sm" onClick={() => void saveAndTest()} disabled={busy || !complete}>
                {busy && <Loader2 size={14} className="animate-spin" />}保存并测试连接
              </Button>
            )}
            {configured && (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                onClick={() => {
                  setForm(formFromStatus(status));
                  setEditing(false);
                }}
              >
                取消
              </Button>
            )}
          </div>
        </div>
      )}

      {error && (
        <div className="mt-2 text-xs text-red-500" role="alert" data-testid="sync-connection-error">
          {error}
        </div>
      )}

      {probe && (
        <ul className="mt-2 space-y-1 text-xs" data-testid="sync-probe">
          <li className="text-muted-foreground">最近一次测试连接的结果：</li>
          <li className={probe.read.ok ? TONE_CLASS.ok : TONE_CLASS.error}>
            {probe.read.ok ? "✓ 连接与读取：" : "✗ 连接与读取："}
            {probe.read.message}
          </li>
          <li className={probe.write.ok ? TONE_CLASS.ok : TONE_CLASS.error}>
            {probe.write.ok ? "✓ 写入权限：" : "✗ 写入权限："}
            {probe.write.message}
          </li>
          {probe.concurrency && (
            <li
              className={probe.concurrency.mode === "conditional" ? TONE_CLASS.ok : TONE_CLASS.warn}
            >
              {probe.concurrency.mode === "conditional"
                ? "✓ 并发保护：支持"
                : "⚠ 并发保护：不支持（退回到“写之前再读一次比较”）"}
              <span className="text-muted-foreground">
                {" "}
                （{probe.concurrency.notes.join("；")}）
              </span>
            </li>
          )}
        </ul>
      )}

      {notice && configured && (
        <div
          className={cn("mt-2 text-xs", TONE_CLASS[notice.tone])}
          data-testid="sync-concurrency-notice"
        >
          {notice.text}
        </div>
      )}
    </SectionCard>
  );
}
