// Adapted from Netcatty components/cloud-sync/CloudSyncControls.tsx (GatekeeperScreen, master key setup)
//! 主密码：第一次设置（输入两次、强度、“我知道了”）、第二台电脑输入主密码解开云端数据、
//! 需要重新输入、记住开关、修改主密码。

import { Loader2 } from "lucide-react";
import { useState } from "react";

import {
  syncRememberPassword,
  syncSetPassword,
  type SyncStatusView,
} from "../../../api/sync";
import { useSync } from "../../../stores/sync";
import { useVault } from "../../../stores/vault";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { Switch } from "../../ui/switch";
import { SectionCard, SettingRow } from "../settingsUi";
import { ChangePasswordDialog } from "./SyncDialogs";
import { StrengthBar } from "./StrengthBar";

export default function PasswordCard({ status }: { status: SyncStatusView }) {
  const probe = useSync((state) => state.probe);
  const apply = useSync((state) => state.apply);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [understood, setUnderstood] = useState(false);
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);

  if (!status.configured) return null;

  // 云端已经有数据 = 第二台电脑：输入当初设置的主密码，不是新设一个。
  // 没测试过连接就不知道云端有没有数据，此时不给输入框（见下面的提示）。
  const cloudHasData = probe?.has_existing_data ?? false;
  const creating = !status.ready && probe !== null && !probe.has_existing_data;
  const needsEntry = !status.ready || !status.password_in_memory;

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      const result = await syncSetPassword(
        password,
        remember,
        creating ? confirm : undefined,
      );
      apply(result.status_view);
      setPassword("");
      setConfirm("");
      setUnderstood(false);
      // 同步可能写入了云端数据：主机列表要重新读。
      void useVault.getState().load();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function toggleRemember(next: boolean) {
    setError(null);
    try {
      apply(await syncRememberPassword(next));
    } catch (err: unknown) {
      setError(String(err));
    }
  }

  const canSubmit = creating
    ? password.length > 0 && confirm.length > 0 && understood
    : password.length > 0;

  return (
    <SectionCard
      title="主密码"
      description="同步包用主密码加密后才上传，云端（和存储服务商）看不到你的任何内容。"
    >
      {!status.ready && probe === null ? (
        <div className="py-2 text-xs text-muted-foreground" data-testid="sync-password-needs-test">
          先点上面的“测试连接”：要先知道云端有没有数据，才能决定是“设置新的主密码”还是“输入已有的主密码”。
        </div>
      ) : needsEntry ? (
        <div className="space-y-3 py-2" data-testid="sync-password-form">
          {creating ? (
            <>
              <div
                className="rounded-md border border-red-500/50 bg-red-500/10 p-3 text-xs"
                role="alert"
              >
                <strong className="text-red-500">忘记主密码就无法恢复云端数据。</strong>
                主密码不会上传，也没有“找回密码”——忘了只能删掉云端数据重新开始。请把它记在安全的地方。
              </div>
              <Input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="设置主密码（至少 8 个字符）"
                aria-label="主密码"
                autoComplete="new-password"
              />
              <StrengthBar password={password} />
              <Input
                type="password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                placeholder="再输入一次"
                aria-label="确认主密码"
                autoComplete="new-password"
              />
              <label className="flex cursor-pointer items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={understood}
                  onChange={(e) => setUnderstood(e.target.checked)}
                  aria-label="我知道忘记主密码就无法恢复云端数据"
                />
                我知道忘记主密码就无法恢复云端数据
              </label>
            </>
          ) : (
            <>
              <div className="text-xs text-muted-foreground">
                {cloudHasData && !status.ready
                  ? "云端已经有同步数据。输入当初设置的主密码来解开它（这台电脑上的主机会和云端合并，两边都不会丢）。"
                  : "需要主密码才能继续同步。"}
              </div>
              <Input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="输入主密码"
                aria-label="主密码"
                autoComplete="current-password"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && canSubmit && !busy) void submit();
                }}
              />
            </>
          )}
          <label className="flex cursor-pointer items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={remember}
              onChange={(e) => setRemember(e.target.checked)}
              aria-label="在这台电脑上记住主密码"
            />
            在这台电脑上记住主密码（用 Windows 账户加密保存，下次不用再输入）
          </label>
          <div className="flex gap-2">
            <Button size="sm" onClick={() => void submit()} disabled={busy || !canSubmit}>
              {busy && <Loader2 size={14} className="animate-spin" />}
              {creating ? "设置主密码并同步" : "解开并同步"}
            </Button>
          </div>
        </div>
      ) : (
        <>
          <SettingRow
            title="在这台电脑上记住主密码"
            description="用 Windows 账户加密保存在本机。关掉会删除保存的那份，下次启动需要重新输入。"
          >
            <Switch
              checked={status.password_remembered}
              onCheckedChange={(next) => void toggleRemember(next)}
              aria-label="在这台电脑上记住主密码"
            />
          </SettingRow>
          <SettingRow
            title="修改主密码"
            description="需要旧密码。会用新密码重新加密云端数据；其他电脑之后要输入新的主密码。"
          >
            <Button size="sm" variant="outline" onClick={() => setChanging(true)}>
              修改…
            </Button>
          </SettingRow>
        </>
      )}
      {error && (
        <div className="mt-2 text-xs text-red-500" role="alert" data-testid="sync-password-error">
          {error}
        </div>
      )}
      <ChangePasswordDialog open={changing} onOpenChange={setChanging} />
    </SectionCard>
  );
}
