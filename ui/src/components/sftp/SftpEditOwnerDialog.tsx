//! 编辑器保存时发现"保存后文件的属主会改变"（登录用户没权限把属主改回服务账号）：
//! 什么都还没替换，先问用户（N05）。

import type { SftpEditEvent } from "../../api/sftp";
import { ownerPromptText } from "../../state/editRecovery";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";

export type OwnerChoice = "owner_keep_new" | "owner_in_place";

export interface SftpEditOwnerDialogProps {
  event: SftpEditEvent;
  /** 选了"仍然保存"或"直接写入原文件"。 */
  onChoose: (choice: OwnerChoice) => void;
  /** 取消：什么都不改，这次修改仍然留在本地、算没上传。 */
  onCancel: () => void;
}

export function SftpEditOwnerDialog({
  event,
  onChoose,
  onCancel,
}: SftpEditOwnerDialogProps) {
  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[560px]" data-section="edit-owner">
        <DialogHeader>
          <DialogTitle>保存后文件的属主会改变</DialogTitle>
          <DialogDescription className="break-all">
            {event.remote_path}
          </DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground" data-owner-text>
          {ownerPromptText(event)}
        </p>
        <div className="space-y-2 text-sm">
          <p>
            <b>仍然保存（属主改变）</b>
            ：用临时文件替换原文件，中途断线也不会弄残原文件，但属主会变成上面说的那个，
            用原属主运行的服务可能读不了它。
          </p>
          <p>
            <b>直接写入原文件</b>
            ：属主和权限都不变，但是写到一半断线或者磁盘写满的话，服务器上的文件可能只剩半截。
          </p>
        </div>
        <DialogFooter className="flex-wrap gap-2 sm:justify-between">
          <Button variant="ghost" onClick={onCancel}>
            取消
          </Button>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              onClick={() => onChoose("owner_in_place")}
            >
              直接写入原文件
            </Button>
            <Button
              variant="destructive"
              onClick={() => onChoose("owner_keep_new")}
            >
              仍然保存（属主改变）
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export default SftpEditOwnerDialog;
