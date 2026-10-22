//! 主机指纹弹窗（参照 Netcatty components/PassphraseModal.tsx 的样式重做）。
//!
//! 未知主机：信任并保存 / 仅本次 / 取消。
//! 指纹变化：只给"关闭"，不提供继续，而且要用红色警告说清楚（设计文档 4.4）。

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
  const target = prompt.port === 22 ? prompt.host : `${prompt.host}:${prompt.port}`;

  return (
    <Dialog open onOpenChange={(open) => !open && onAnswer("reject")}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {changed ? (
              <>
                <ShieldAlert className="h-5 w-5 text-destructive" />
                主机指纹发生变化
              </>
            ) : (
              <>
                <ShieldCheck className="h-5 w-5 text-primary" />
                第一次连接这台主机
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {changed
              ? "已经阻断这次连接。"
              : prompt.status === "other_key_types_saved"
                ? "这台主机之前保存的是别的类型的密钥，这次用的是新的类型。"
                : "确认指纹没问题再继续。"}
          </DialogDescription>
        </DialogHeader>

        {changed ? (
          <div className="rounded-lg border-2 border-destructive bg-destructive/10 px-4 py-3 text-sm">
            <p className="font-semibold text-destructive">
              这台服务器的密钥和上次不一样了，连接已被阻断。
            </p>
            <p className="mt-1.5 text-foreground/80">
              可能是服务器重装或者换了密钥，也可能是有人在中间冒充服务器。确认过原因之后，
              去主机详情的"高级"里点"清除已保存的指纹"，再连一次。
            </p>
          </div>
        ) : null}

        <dl className="space-y-2 text-sm">
          <Row label="主机" value={target} />
          <Row label="密钥类型" value={prompt.key_type} />
          <Row label={changed ? "本次收到的指纹" : "指纹"} value={prompt.fingerprint} mono />
        </dl>

        <DialogFooter>
          {changed ? (
            <Button variant="outline" onClick={() => onAnswer("reject")}>
              关闭
            </Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onAnswer("reject")}>
                取消
              </Button>
              <Button variant="secondary" onClick={() => onAnswer("trust_once")}>
                仅本次
              </Button>
              <Button onClick={() => onAnswer("trust_and_save")}>信任并保存</Button>
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
