//! 上次没上传的修改：启动时问一次，点了"稍后处理"以后这次运行里仍然有一个常驻入口
//! （"N 个未上传的修改"），点开就是恢复对话框（N04）。
//!
//! 重新上传用这台主机上**任何一个已连好的 SFTP 会话**（SFTP 标签、终端旁边的文件侧栏都算），
//! 所以用户可以先点"稍后处理"，连上主机以后再回来处理。

import { useCallback, useEffect, useState } from "react";

import type { EditLeftover, EditLeftoverResult } from "../../api/sftp";
import { sftpSessionFor, type SftpSessionInfo } from "../../state/editRecovery";
import { EditRecoveryDialog, type RecoveryAction } from "./EditRecoveryDialog";

export interface EditRecoveryHostProps {
  /** 现在在用的 SFTP 会话（标签 + 文件侧栏）。 */
  sessions: ReadonlyArray<SftpSessionInfo>;
  /** 读上次留下的修改。 */
  load: () => Promise<EditLeftover[]>;
  /** 处理一项；`sftpId` 是这台主机上已连好的会话（没有就是 `undefined`）。 */
  act: (
    item: EditLeftover,
    action: RecoveryAction,
    sftpId: string | undefined,
  ) => Promise<EditLeftoverResult>;
}

export function EditRecoveryHost({
  sessions,
  load,
  act,
}: EditRecoveryHostProps) {
  const [items, setItems] = useState<EditLeftover[]>([]);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void load()
      .then((loaded) => {
        if (cancelled) return;
        setItems(loaded);
        setOpen(loaded.length > 0);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // 只在启动时读一次；之后的变化（处理掉一项）由本组件自己维护。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onAction = useCallback(
    async (item: EditLeftover, action: RecoveryAction) => {
      const sftpId = sftpSessionFor(sessions, item.host_id) ?? undefined;
      const result = await act(item, action, sftpId);
      if (result.state === "done") {
        setItems((current) => {
          const next = current.filter((entry) => entry.id !== item.id);
          if (next.length === 0) setOpen(false);
          return next;
        });
      }
      return result;
    },
    [act, sessions],
  );

  if (items.length === 0) return null;
  return (
    <>
      {!open && (
        <button
          type="button"
          data-testid="edit-recovery-entry"
          onClick={() => setOpen(true)}
          className="flex w-full items-center gap-2 border-t border-amber-600/40 bg-amber-500/10 px-3 py-1 text-left text-xs text-amber-600 hover:bg-amber-500/20"
        >
          <span className="h-2 w-2 shrink-0 rounded-full bg-amber-500" />
          <span>{items.length} 个未上传的修改，点这里处理</span>
        </button>
      )}
      {open && (
        <EditRecoveryDialog
          items={items}
          sessions={sessions}
          onAction={onAction}
          onLater={() => setOpen(false)}
        />
      )}
    </>
  );
}

export default EditRecoveryHost;
