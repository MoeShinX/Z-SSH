//! 连接存储里 OneDrive 的几块界面：登录面板（未连接 / 登录中）、已连接的摘要、撤销授权的提示。
//!
//! 流程：点“用微软账号登录”→ Z-SSH 用系统默认浏览器打开微软登录页 → 用户登录并同意 →
//! 浏览器自动跳回本机，页面显示“授权成功”→ Z-SSH 窗口回到前台，显示已连接的账号。
//! 登录中显示“正在浏览器中登录…”和“取消”。浏览器那一步用的是系统浏览器自己的网络设置，
//! 登录之后 Z-SSH 换令牌和同步走“连接 → 软件网络”里设置的代理，界面上用一句话说明。

import { Loader2 } from "lucide-react";
import { useState } from "react";

import { syncOnedriveOpenRevokePage, type SyncStatusView } from "../../../api/sync";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";

/** Azure 的应用程序(客户端) ID 是 GUID：8-4-4-4-12 个十六进制字符。 */
export function isValidClientId(text: string): boolean {
  return /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(
    text.trim(),
  );
}

/** 登录能不能点：编进了 client ID，或者“高级”里填了格式正确的。 */
export function canLogin(builtin: boolean, customClientId: string): boolean {
  const custom = customClientId.trim();
  if (custom !== "") return isValidClientId(custom);
  return builtin;
}

/** 登录中：提示 + 取消。 */
export function OnedriveLoggingIn({ onCancel }: { onCancel: () => void }) {
  return (
    <div
      className="space-y-2 rounded-md border border-border/60 bg-muted/30 p-3 text-xs"
      data-testid="onedrive-logging-in"
      role="status"
    >
      <div className="flex items-center gap-2 text-[13px] font-medium">
        <Loader2 size={14} className="animate-spin" />
        正在浏览器中登录…
      </div>
      <div className="leading-relaxed text-muted-foreground">
        请在刚刚打开的浏览器页面里登录并同意授权，最多等 5 分钟。完成后浏览器会自动回到本机，
        显示“授权成功”，Z-SSH 窗口也会回到前台。浏览器这一步用的是系统浏览器自己的网络设置；
        登录之后 Z-SSH 换令牌和同步走「连接」→「软件网络」里设置的代理。
      </div>
      <Button size="sm" variant="outline" onClick={onCancel}>
        取消
      </Button>
    </div>
  );
}

interface LoginPanelProps {
  status: SyncStatusView | null;
  clientId: string;
  setClientId: (value: string) => void;
  loggingIn: boolean;
  onLogin: () => void;
  onCancel: () => void;
  /** 刚刚断开了连接：提示怎么彻底撤销授权。 */
  justDisconnected: boolean;
}

/** 未连接时的 OneDrive 表单：说明、登录按钮、“高级”里的 client ID。 */
export function OnedriveLoginPanel({
  status,
  clientId,
  setClientId,
  loggingIn,
  onLogin,
  onCancel,
  justDisconnected,
}: LoginPanelProps) {
  const builtin = status?.onedrive_builtin_client ?? false;
  const custom = clientId.trim();
  const customInvalid = custom !== "" && !isValidClientId(custom);
  const allowed = canLogin(builtin, clientId);

  return (
    <div className="space-y-3" data-testid="onedrive-fields">
      <div className="rounded-md border border-border/60 bg-muted/30 p-3 text-xs leading-relaxed">
        <div className="font-medium">用微软账号登录</div>
        点“登录”后，Z-SSH 会用系统默认浏览器打开微软的登录页；登录并同意后浏览器会自动跳回本机，
        不用复制任何东西。同步包只放在 OneDrive 的<strong>应用专用文件夹</strong>
        （OneDrive 里 <code>Apps</code> 下以应用名命名的文件夹）里，Z-SSH 看不到你的其他文件。
        目前只支持<strong>个人微软账号</strong>（outlook.com、hotmail.com 等），工作或学校账号暂不支持。
      </div>

      {justDisconnected && <RevokeHint prefix="已断开连接：本机的登录信息已删除。" />}

      {loggingIn ? (
        <OnedriveLoggingIn onCancel={onCancel} />
      ) : (
        <div className="space-y-2">
          <div className="flex items-center gap-3">
            <Button size="sm" onClick={onLogin} disabled={!allowed}>
              用微软账号登录
            </Button>
          </div>
          {!allowed && !customInvalid && (
            <div className="text-xs text-amber-500" role="alert" data-testid="onedrive-no-client">
              这个版本没有内置 OneDrive 的 client ID（自己从源码编译的版本是这样），登录按钮暂时不可用。
              在下面的“高级”里填你自己在 Azure 注册的应用的 client ID 就能用。
            </div>
          )}
          {customInvalid && (
            <div className="text-xs text-amber-500" role="alert">
              client ID 的格式不对，应该形如 xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx。
            </div>
          )}
        </div>
      )}

      <details className="text-xs" data-testid="onedrive-advanced">
        <summary className="cursor-pointer text-muted-foreground">高级</summary>
        <label className="mt-2 block space-y-1">
          <span className="text-xs text-muted-foreground">自己的 client ID（可选）</span>
          <Input
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
            aria-label="client ID"
            autoComplete="off"
            disabled={loggingIn}
          />
          <span className="block text-[11px] text-muted-foreground">
            填了就用它登录，不用程序里内置的。client ID 只存在这台电脑上，不会同步。注册应用的步骤见项目文档
            <code> docs/release.md</code>。
          </span>
        </label>
      </details>
    </div>
  );
}

/** 怎么彻底撤销授权：个人账号没有简单的撤销接口，让用户去微软账户的“应用和服务”页面。 */
export function RevokeHint({ prefix }: { prefix?: string }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-1 text-xs text-muted-foreground" data-testid="onedrive-revoke-hint">
      <div>
        {prefix} 断开连接只删除这台电脑上保存的登录信息。如果还要彻底撤销 Z-SSH 的授权，请在微软账户的
        “应用和服务”页面里移除它。
      </div>
      <Button
        size="sm"
        variant="outline"
        onClick={() => {
          setError(null);
          syncOnedriveOpenRevokePage().catch((err: unknown) => setError(String(err)));
        }}
      >
        在浏览器中打开“应用和服务”页面
      </Button>
      {error && (
        <div className="text-red-500" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}

interface SummaryProps {
  status: SyncStatusView;
  busy: boolean;
  loggingIn: boolean;
  onTest: () => void;
  onRelogin: () => void;
  onCancelLogin: () => void;
  onDisconnect: () => void;
}

/** 已连接：账号、位置、登录失效提示，以及 测试连接 / 重新登录 / 断开连接。 */
export function OnedriveSummary({
  status,
  busy,
  loggingIn,
  onTest,
  onRelogin,
  onCancelLogin,
  onDisconnect,
}: SummaryProps) {
  const config = status.config;
  const [confirming, setConfirming] = useState(false);
  if (!config) return null;
  return (
    <div className="space-y-2" data-testid="onedrive-summary">
      <div>
        <span className="text-muted-foreground">类型：</span>OneDrive（个人微软账号）
      </div>
      <div data-testid="onedrive-account">
        <span className="text-muted-foreground">账号：</span>
        {config.account || "Microsoft 账号"}
      </div>
      <div>
        <span className="text-muted-foreground">存放位置：</span>
        OneDrive 里 <code>Apps</code> 下的应用专用文件夹（只放加密后的同步包）
      </div>
      {config.client_id && (
        <div className="break-all text-xs text-muted-foreground">
          使用自己填的 client ID：{config.client_id}
        </div>
      )}
      <div className="text-xs text-muted-foreground">
        登录信息（令牌）用本机的 Windows 账户加密保存，不会进同步包。
      </div>
      {status.auth_paused && (
        <div
          className="rounded-md border border-red-500/50 bg-red-500/10 p-2 text-xs text-red-500"
          role="alert"
          data-testid="onedrive-login-expired"
        >
          OneDrive 登录已失效，请重新登录。自动同步已暂停，重新登录之后恢复。
        </div>
      )}

      {loggingIn ? (
        <OnedriveLoggingIn onCancel={onCancelLogin} />
      ) : confirming ? (
        <div className="space-y-2 rounded-md border border-border/60 p-2 text-xs">
          <div>断开后这台电脑不再同步到 OneDrive，云端已有的同步包不会被删除。确定断开吗？</div>
          <div className="flex gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setConfirming(false);
                onDisconnect();
              }}
              disabled={busy}
            >
              确定断开
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirming(false)}>
              取消
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-wrap gap-2 pt-1">
          <Button size="sm" variant="outline" onClick={onTest} disabled={busy}>
            {busy && <Loader2 size={14} className="animate-spin" />}测试连接
          </Button>
          <Button
            size="sm"
            variant={status.auth_paused ? "default" : "ghost"}
            onClick={onRelogin}
            disabled={busy}
          >
            重新登录
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setConfirming(true)} disabled={busy}>
            断开连接
          </Button>
        </div>
      )}
      <RevokeHint />
    </div>
  );
}
