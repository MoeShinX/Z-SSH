//! 解压遇到同名文件时的对话框（UI-13）：显示冲突个数和前几个名字，让用户选
//! 「覆盖」「跳过已有文件」「取消」。风格和同名文件对话框（`SftpConflictDialog`）一致。

import { useRef } from "react";

import type { ConflictPolicy, ExtractPlan } from "../../api/sftp";
import { conflictMoreText } from "../../state/sftpArchive";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";

interface Props {
  /** 压缩包的文件名。 */
  archiveName: string;
  /** 解压到哪个文件夹（只写名字）。 */
  destName: string;
  plan: Pick<ExtractPlan, "conflict_count" | "conflict_sample">;
  onChoose: (policy: ConflictPolicy) => void;
  /** 取消（按钮、Esc、点遮罩都算）：什么都不解压。 */
  onCancel: () => void;
}

export default function SftpExtractConflictDialog({
  archiveName,
  destName,
  plan,
  onChoose,
  onCancel,
}: Props) {
  const skipRef = useRef<HTMLButtonElement | null>(null);
  const more = conflictMoreText(plan);
  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent
        className="sm:max-w-[480px]"
        data-testid="extract-conflict"
        // 打开时焦点落在不会丢数据的「跳过已有文件」上：直接按回车不会覆盖任何东西
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          skipRef.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>目标里已有同名文件</DialogTitle>
          <DialogDescription className="break-all">
            解压「{archiveName}」到「{destName}」时，有 {plan.conflict_count} 个文件在里面已经存在。
          </DialogDescription>
        </DialogHeader>
        <ul
          data-section="extract-conflict-names"
          className="max-h-48 space-y-0.5 overflow-y-auto rounded-md border border-border/60 bg-muted/40 px-3 py-2 font-mono text-xs"
        >
          {plan.conflict_sample.map((name, index) => (
            <li key={`${index}-${name}`} className="truncate" title={name}>
              {name}
            </li>
          ))}
          {more && (
            <li data-section="extract-conflict-more" className="text-muted-foreground">
              …{more}
            </li>
          )}
        </ul>
        <DialogFooter>
          <Button variant="outline" data-action="extract-cancel" onClick={onCancel}>
            取消
          </Button>
          <Button
            ref={skipRef}
            variant="secondary"
            data-action="extract-skip"
            onClick={() => onChoose("skip_existing")}
          >
            跳过已有文件
          </Button>
          <Button data-action="extract-overwrite" onClick={() => onChoose("overwrite")}>
            覆盖
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
