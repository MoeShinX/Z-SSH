//! 主机指纹弹窗（参照 Netcatty components/PassphraseModal.tsx 的样式重做）。
//!
//! 未知主机：信任并保存 / 仅本次 / 取消。
//! 指纹变化：红色警告，列出旧指纹和新指纹；默认按钮是"关闭"，只有用户明确点
//! "更新指纹并连接"才继续（服务器重装 / DD 之后最常见，不用再去设置里清指纹、重输密码）。
//! 密钥被吊销：只给"关闭"。

import { ShieldAlert, ShieldCheck } from "lucide-react";

import type { HostKeyAnswer, HostKeyPrompt } from "../../api/auth";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";

interface HostKeyDialogProps {
  prompt: HostKeyPrompt;
  onAnswer: (decision: HostKeyAnswer) => void;
}

export default function HostKeyDialog({ prompt, onAnswer }: HostKeyDialogProps) {
  const changed = prompt.status === "changed";
  const revoked = prompt.status === "revoked";
  const alarming = changed || revoked;
  const target = prompt.port === 22 ? prompt.host : `${prompt.host}:${prompt.port}`;

  return (
    <Dialog open onOpenChange={(open) => !open && onAnswer("reject")}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {alarming ? (
              <>
                <ShieldAlert className="h-5 w-5 text-destructive" />
                {revoked ? "主机密钥已被吊销" : "主机指纹发生变化"}
              </>
            ) : (
              <>
                <ShieldCheck className="h-5 w-5 text-primary" />
                第一次连接这台主机
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {revoked
              ? "已经阻断这次连接。"
              : changed
                ? "确认原因之前不要继续。"
                : prompt.status === "other_key_types_saved"
                  ? "这台主机之前保存的是别的类型的密钥，这次用的是新的类型。"
                  : "确认指纹没问题再继续。"}
          </DialogDescription>
        </DialogHeader>

        {changed ? (
          <div
            data-section="hostkey-changed-warning"
            className="rounded-lg border-2 border-destructive bg-destructive/10 px-4 py-3 text-sm"
          >
            <p className="font-semibold text-destructive">这台服务器的密钥和上次保存的不一样了。</p>
            <p className="mt-1.5 text-foreground/80">
              如果你刚重装过系统、DD 过或者换过 SSH 密钥，这是正常的，点"更新指纹并连接"，
              会用新指纹替换旧记录并用保存的密码继续连接。如果没有做过这些，可能是有人在中间冒充服务器，
              请先关闭，查清楚再连。
            </p>
          </div>
        ) : revoked ? (
          <div className="rounded-lg border-2 border-destructive bg-destructive/10 px-4 py-3 text-sm">
            <p className="font-semibold text-destructive">
              这把密钥在 known_hosts 里被标记为已吊销（@revoked），连接已被阻断。
            </p>
            <p className="mt-1.5 text-foreground/80">吊销是有人特意标记的，这里不提供继续。</p>
          </div>
        ) : null}

        <dl className="space-y-2 text-sm">
          <Row label="主机" value={target} />
          <Row label="密钥类型" value={prompt.key_type} />
          {alarming && prompt.stored_fingerprint ? (
            <Row label="已保存的指纹" value={prompt.stored_fingerprint} mono />
          ) : null}
          <Row label={alarming ? "本次收到的指纹" : "指纹"} value={prompt.fingerprint} mono />
        </dl>

        <DialogFooter>
          {revoked ? (
            <Button variant="outline" data-action="reject" onClick={() => onAnswer("reject")}>
              关闭
            </Button>
          ) : changed ? (
            <>
              <Button variant="outline" data-action="reject" onClick={() => onAnswer("reject")}>
                关闭
              </Button>
              <Button
                variant="destructive"
                data-action="replace"
                onClick={() => onAnswer("trust_and_save")}
              >
                更新指纹并连接
              </Button>
            </>
          ) : (
            <>
              <Button variant="outline" data-action="reject" onClick={() => onAnswer("reject")}>
                取消
              </Button>
              <Button
                variant="secondary"
                data-action="trust-once"
                onClick={() => onAnswer("trust_once")}
              >
                仅本次
              </Button>
              <Button data-action="trust-and-save" onClick={() => onAnswer("trust_and_save")}>
                信任并保存
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex gap-3">
      <dt className="w-28 shrink-0 text-muted-foreground">{label}</dt>
      <dd className={`min-w-0 flex-1 break-all ${mono ? "font-mono text-xs" : ""}`}>
        {value}
      </dd>
    </div>
  );
}
