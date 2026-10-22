// Adapted from Netcatty components/cloud-sync/CloudSyncDashboardTabs.tsx (GitHub provider card)
//! 连接存储里 GitHub Gist 的那一块表单：令牌、Gist ID（填已有的，或者新建一个 secret Gist）、
//! 接口地址（可选，GitHub Enterprise 用）。
//!
//! 文案要如实：**Gist 没有真正“私有”的**。secret Gist 不会出现在公开列表和搜索里，但任何拿到
//! 链接的人都能打开它——同步包是密文，所以内容安全，但不能写成“私有 Gist”。

import { Loader2 } from "lucide-react";

import { Button } from "../../ui/button";
import { Input } from "../../ui/input";

export interface GithubFormValue {
  apiBase: string;
  token: string;
  gistId: string;
  allowHttp: boolean;
}

interface Props {
  form: GithubFormValue;
  set: <K extends keyof GithubFormValue>(key: K, value: GithubFormValue[K]) => void;
  /** 本机已经保存过令牌（表单里令牌留空就沿用它）。 */
  savedToken: boolean;
  plainHttp: boolean;
  creating: boolean;
  busy: boolean;
  onCreate: () => void;
}

export default function GithubFields({
  form,
  set,
  savedToken,
  plainHttp,
  creating,
  busy,
  onCreate,
}: Props) {
  const canCreate =
    (form.token.trim() !== "" || savedToken) &&
    form.gistId.trim() === "" &&
    !creating &&
    !busy &&
    (!plainHttp || form.allowHttp);

  return (
    <div className="space-y-3" data-testid="github-fields">
      <div
        className="rounded-md border border-border/60 bg-muted/30 p-3 text-xs leading-relaxed"
        data-testid="github-secret-gist-note"
      >
        <div className="font-medium">关于 Gist 的“私密”程度</div>
        同步包先用主密码整体加密，再放进一个 <strong>secret Gist</strong>。secret Gist
        不会出现在你的公开主页和搜索里，但<strong>任何拿到它链接的人都能打开它</strong>
        ——打开看到的是密文，没有主密码解不开。所以内容是安全的，但不要把 Gist 的链接发给别人。
        Z-SSH 只会改里面的 <code>zssh-sync.json</code> 这一个文件。
      </div>

      <label className="block space-y-1">
        <span className="text-xs text-muted-foreground">个人访问令牌</span>
        <Input
          type="password"
          value={form.token}
          onChange={(e) => set("token", e.target.value)}
          aria-label="GitHub 令牌"
          autoComplete="off"
          placeholder={savedToken ? "已保存（出于安全不显示），留空 = 继续用它" : "粘贴令牌"}
        />
        <span className="block text-[11px] text-muted-foreground">
          令牌只用本机的 Windows 账户加密保存，不会进同步包，保存之后界面也不再显示它。
        </span>
      </label>

      <div
        className="rounded-md border border-border/60 p-3 text-xs leading-relaxed"
        data-testid="github-token-help"
      >
        <div className="font-medium">怎么创建令牌</div>
        <ol className="mt-1 list-decimal space-y-1 pl-5">
          <li>
            在浏览器里登录 GitHub，打开 <code>github.com/settings/tokens</code>
            （头像 → Settings → Developer settings → Personal access tokens）。
          </li>
          <li>
            <strong>经典令牌</strong>（Tokens (classic) → Generate new token (classic)）：只勾选{" "}
            <code>gist</code> 这一项权限。
          </li>
          <li>
            <strong>细粒度令牌</strong>（Fine-grained tokens）：Account permissions →{" "}
            <code>Gists</code> → <code>Read and write</code>；Repository access 随便选，用不到。
          </li>
          <li>
            建议设一个过期时间。令牌过期或被撤销后，同步会提示“令牌已失效，请重新填写”，
            到这里填新令牌即可。
          </li>
        </ol>
      </div>

      <div className="space-y-1">
        <label className="block space-y-1">
          <span className="text-xs text-muted-foreground">Gist ID</span>
          <div className="flex gap-2">
            <Input
              value={form.gistId}
              onChange={(e) => set("gistId", e.target.value)}
              aria-label="Gist ID"
              placeholder="已有的 Gist 填 ID；没有就点右边新建"
              autoComplete="off"
            />
            <Button
              type="button"
              variant="outline"
              onClick={onCreate}
              disabled={!canCreate}
              className="shrink-0"
            >
              {creating && <Loader2 size={14} className="animate-spin" />}新建 Gist
            </Button>
          </div>
        </label>
        <span className="block text-[11px] text-muted-foreground">
          已有 Gist：填它的 ID（Gist 地址最后那一段）。没有：点“新建 Gist”，Z-SSH 会新建一个
          secret Gist（描述“Z-SSH 配置同步（已加密）”），ID 会显示出来——
          <strong>第二台电脑上填同一个 ID</strong>（令牌可以用同一个，也可以是另一个有权限的令牌）。
        </span>
      </div>

      <label className="block space-y-1">
        <span className="text-xs text-muted-foreground">接口地址（可选）</span>
        <Input
          value={form.apiBase}
          onChange={(e) => set("apiBase", e.target.value)}
          aria-label="接口地址"
          placeholder="https://api.github.com"
        />
        <span className="block text-[11px] text-muted-foreground">
          留空 = GitHub.com。GitHub Enterprise 填 <code>https://你的域名/api/v3</code>。
        </span>
      </label>

      {plainHttp && (
        <div
          className="space-y-2 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-xs"
          role="alert"
        >
          <div className="font-medium text-amber-500">接口地址是 http://，令牌会以明文发送</div>
          <div>同一网络里的人可以看到你的令牌，拿到它就能读写你的 Gist。只在可信的内网里这样用。</div>
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={form.allowHttp}
              onChange={(e) => set("allowHttp", e.target.checked)}
              aria-label="我了解令牌会以明文发送"
            />
            我了解令牌会以明文发送，仍然使用 http://
          </label>
        </div>
      )}
    </div>
  );
}
