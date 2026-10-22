//! 密码 / 私钥口令 / keyboard-interactive 的提问弹窗
//! （参照 Netcatty components/PassphraseModal.tsx 和 KeyboardInteractiveModal.tsx 的样式）。

import { KeyRound, MessageSquareText, Play } from "lucide-react";
import { useEffect, useState } from "react";

import type { AuthPrompt } from "../../api/auth";
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
import { Switch } from "../ui/switch";

interface AuthDialogProps {
  prompt: AuthPrompt;
  /** answers 为 null 表示取消 */
  onAnswer: (answers: string[] | null, remember: boolean) => void;
}

const KIND_TITLES: Record<AuthPrompt["kind"], string> = {
  password: "需要密码",
  passphrase: "需要私钥口令",
  keyboard_interactive: "服务器要求回答",
};

const KIND_DESCRIPTIONS: Record<AuthPrompt["kind"], string> = {
  password: "这台主机没有保存密码，输入之后继续。",
  passphrase: "私钥被口令保护着，输入口令来解锁。",
  keyboard_interactive: "服务器发来的提问，原样回答就行。",
};

export default function AuthDialog({ prompt, onAnswer }: AuthDialogProps) {
  const [answers, setAnswers] = useState<string[]>(() => prompt.prompts.map(() => ""));
  const [remember, setRemember] = useState(prompt.can_remember);
  const [busy, setBusy] = useState(false);

  // 换一道题就重置输入
  useEffect(() => {
    setAnswers(prompt.prompts.map(() => ""));
    setRemember(prompt.can_remember);
    setBusy(false);
  }, [prompt]);

  const setAnswer = (index: number, value: string) => {
    setAnswers((previous) => {
      const next = [...previous];
      next[index] = value;
      return next;
    });
  };

  const submit = () => {
    setBusy(true);
    onAnswer(answers, prompt.can_remember && remember);
  };

  const Icon = prompt.kind === "keyboard_interactive" ? MessageSquareText : KeyRound;

  return (
    <Dialog open onOpenChange={(open) => !open && onAnswer(null, false)}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Icon className="h-5 w-5 text-primary" />
            {KIND_TITLES[prompt.kind]}
          </DialogTitle>
          <DialogDescription>{KIND_DESCRIPTIONS[prompt.kind]}</DialogDescription>
        </DialogHeader>

        {prompt.name !== "" && (
          <p className="text-sm font-medium">{prompt.name}</p>
        )}
        {prompt.instructions !== "" && (
          <p className="-mt-2 whitespace-pre-wrap text-xs text-muted-foreground">
            {prompt.instructions}
          </p>
        )}

        <div className="space-y-3">
          {prompt.prompts.map((item, index) => (
            <div key={`${item.text}-${index}`} className="space-y-1.5">
              <Label>{item.text}</Label>
              <Input
                // 回显为否的题目（密码、验证码）用密码框
                type={item.echo ? "text" : "password"}
                value={answers[index] ?? ""}
                autoFocus={index === 0}
                autoComplete="off"
                onChange={(event) => setAnswer(index, event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") submit();
                }}
              />
            </div>
          ))}
        </div>

        {prompt.can_remember && (
          <label className="flex items-center justify-between gap-2 text-[13px]">
            <span className="text-muted-foreground">
              {prompt.kind === "passphrase"
                ? "把口令存进保险库（用 Windows DPAPI 加密）"
                : "把密码存进保险库（用 Windows DPAPI 加密）"}
            </span>
            <Switch checked={remember} onCheckedChange={setRemember} />
          </label>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onAnswer(null, false)}>
            取消
          </Button>
          <Button onClick={submit} disabled={busy}>
            <Play size={14} /> 确定
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
