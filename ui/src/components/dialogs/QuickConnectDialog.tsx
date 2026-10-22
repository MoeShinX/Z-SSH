//! 快速连接的密码弹窗。
//!
//! 上一版的密码是填在顶部那条连接栏里的；M1-07 把连接栏删了（搜索框兼作快速
//! 连接），所以这里在真正连接之前问一次密码。快速连接没有主机记录，"记住密码"
//! 没地方存，所以不提供保存选项（设计上就是一次性的）。

import { Link2 } from "lucide-react";
import { useState } from "react";

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

interface QuickConnectDialogProps {
  target: string;
  onCancel: () => void;
  onConnect: (password: string) => void;
}

export default function QuickConnectDialog({
  target,
  onCancel,
  onConnect,
}: QuickConnectDialogProps) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    onConnect(password);
  };

  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Link2 className="h-5 w-5 text-primary" />
            快速连接
          </DialogTitle>
          <DialogDescription>
            直接连 <span className="font-mono">{target}</span>，不会保存到主机库。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-1.5">
          <Label>密码</Label>
          <Input
            type="password"
            value={password}
            autoFocus
            autoComplete="off"
            onChange={(event) => setPassword(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
            }}
          />
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button onClick={submit} disabled={busy}>
            连接
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
