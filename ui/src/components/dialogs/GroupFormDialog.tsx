//! 新建 / 重命名分组的小弹窗（原 GroupDialog 的样式重做，字段逻辑不动）。

import { useState } from "react";

import type { Group } from "../../api/vault";
import { useSettings } from "../../stores/settings";
import { useVault } from "../../stores/vault";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";

interface GroupFormDialogProps {
  /** null 表示新建 */
  group: Group | null;
  /** 新建时默认挂在哪个分组下 */
  parentId: number | null;
  /** 新建这个分组后，把现在所有未分组的主机都移进去（右键「未分组」→ 重命名）。 */
  adoptUngrouped?: boolean;
  onClose: () => void;
  onError: (message: string) => void;
}

/** 编辑分组时不能把自己或自己的子孙当父分组。 */
function descendantIds(groups: Group[], rootId: number): Set<number> {
  const result = new Set<number>([rootId]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const group of groups) {
      if (group.parent_id !== null && result.has(group.parent_id) && !result.has(group.id)) {
        result.add(group.id);
        changed = true;
      }
    }
  }
  return result;
}

export default function GroupFormDialog({
  group,
  parentId,
  adoptUngrouped = false,
  onClose,
  onError,
}: GroupFormDialogProps) {
  const groups = useVault((state) => state.groups);
  const hosts = useVault((state) => state.hosts);
  const moveHosts = useVault((state) => state.moveHosts);
  const ungroupedIds = hosts.filter((host) => host.group_id === null).map((host) => host.id);
  const saveGroup = useVault((state) => state.saveGroup);

  const [name, setName] = useState(group?.name ?? "");
  const [parent, setParent] = useState<string>(
    group?.parent_id === null || group?.parent_id === undefined
      ? parentId === null
        ? "none"
        : String(parentId)
      : String(group.parent_id),
  );
  const [busy, setBusy] = useState(false);

  const forbidden = group ? descendantIds(groups, group.id) : new Set<number>();

  const submit = async () => {
    if (name.trim() === "") {
      onError("分组名称不能为空");
      return;
    }
    setBusy(true);
    try {
      const saved = await saveGroup({
        id: group?.id ?? null,
        name: name.trim(),
        parent_id: parent === "none" ? null : Number(parent),
        sort: group?.sort ?? 0,
      });
      if (adoptUngrouped && ungroupedIds.length > 0) await moveHosts(ungroupedIds, saved.id);
      // 「未分组」改名成功：树里不再有「未分组」这一行，之后不分组的主机直接排在树的根层（UI-14）。
      // 设置写失败 setUngroupedFlat 自己会提示并回滚，分组已经建好了，不影响关窗口
      if (adoptUngrouped) await useSettings.getState().setUngroupedFlat(true);
      onClose();
    } catch (error: unknown) {
      onError(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{group ? "重命名分组" : adoptUngrouped ? "重命名「未分组」" : "新建分组"}</DialogTitle>
        </DialogHeader>
        {adoptUngrouped && (
          <p className="text-sm text-muted-foreground">
            「未分组」不是一个真的分组，只是没分组的主机放在一起。这里会建一个这个名字的分组，
            把现在 {ungroupedIds.length} 台未分组的主机都移进去。
          </p>
        )}
        <div className="space-y-3">
          <div className="space-y-1.5">
            <Label>名称</Label>
            <Input
              value={name}
              onChange={(event) => setName(event.target.value)}
              autoFocus
              onKeyDown={(event) => {
                if (event.key === "Enter") void submit();
              }}
            />
          </div>
          <div className="space-y-1.5">
            <Label>上级分组</Label>
            <Select value={parent} onValueChange={setParent}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">（顶层）</SelectItem>
                {groups
                  .filter((item) => !forbidden.has(item.id))
                  .map((item) => (
                    <SelectItem key={item.id} value={String(item.id)}>
                      {item.name}
                    </SelectItem>
                  ))}
              </SelectContent>
            </Select>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            取消
          </Button>
          <Button onClick={() => void submit()} disabled={busy}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
