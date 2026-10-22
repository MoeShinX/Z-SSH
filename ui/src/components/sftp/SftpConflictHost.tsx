//! 同名文件对话框的唯一入口（SFTP-6）：一个窗口里挂一个，收这个窗口所有 SFTP 会话（独立标签、
//! 停靠面板、Z-SFTP 窗口里的标签）的提问，按先来后到逐个弹。
//!
//! 为什么不放在各个 `SftpTabView` 里：停靠面板会被卸载（关面板、切到别的主机、切到 SFTP 标签），
//! 面板里的监听跟着没了，后台的传输却还在等回答，等满 2 分钟按「跳过」收场，队列里那一项就
//! 一直停着再变成「已跳过」。这里不依赖面板在不在屏幕上。
//! 事件是广播给所有窗口的，所以要靠 `sessionIds` 认出哪些是本窗口的会话。

import { useEffect, useRef } from "react";

import { onSftpConflict, sftpConflictAnswer } from "../../api/sftp";
import {
  conflictTimeLeft,
  nextConflict,
  useSftpConflicts,
} from "../../state/sftpConflicts";
import { toast } from "../ui/toast";
import { SftpConflictDialog, type ConflictAnswer } from "./SftpDialogs";

interface SftpConflictHostProps {
  /** 本窗口现有的 SFTP 会话 id（独立标签 + 停靠面板的会话）。 */
  sessionIds: readonly string[];
}

export default function SftpConflictHost({ sessionIds }: SftpConflictHostProps) {
  const ownedRef = useRef<ReadonlySet<string>>(new Set());
  ownedRef.current = new Set(sessionIds);
  const owns = (sftpId: string) => ownedRef.current.has(sftpId);

  const queue = useSftpConflicts((state) => state.queue);
  const push = useSftpConflicts((state) => state.push);
  const remove = useSftpConflicts((state) => state.remove);

  // 整个窗口只订阅一次。
  useEffect(() => {
    let cancelled = false;
    let stop: (() => void) | null = null;
    // 不在这里按会话筛：事件广播给所有窗口，会话在窗口之间搬家（停靠面板拆到 Z-SFTP 窗口）时
    // 提问可能正好在路上。全收进队列，弹的时候再看这条提问是不是本窗口现有会话的。
    void onSftpConflict((sftpId, prompt) => push(sftpId, prompt))
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      // 订阅不上（事件通道不可用）时不让整个窗口因为未处理的拒绝而报错
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [push]);

  // 后端等满 2 分钟已经按「跳过」处理了：队列里对应的提问也撤掉（不然用户答了也没人收，
  // 回答还会报错）；本窗口的提问撤掉时说一声，别的窗口的会话不关这里的事。
  useEffect(() => {
    if (queue.length === 0) return;
    const now = Date.now();
    const timers = queue.map((item) =>
      window.setTimeout(() => {
        if (
          useSftpConflicts
            .getState()
            .queue.some((pending) => pending.prompt.conflict_id === item.prompt.conflict_id)
        ) {
          remove(item.prompt.conflict_id);
          if (ownedRef.current.has(item.sftpId)) {
            toast.info("同名文件提问等太久，已按跳过处理");
          }
        }
      }, conflictTimeLeft(item, now)),
    );
    return () => timers.forEach((timer) => window.clearTimeout(timer));
  }, [queue, remove]);

  const head = nextConflict(queue, owns);
  if (!head) return null;

  const answer = (reply: ConflictAnswer) => {
    remove(head.prompt.conflict_id);
    void sftpConflictAnswer(
      head.prompt.conflict_id,
      reply.action,
      reply.applyToAll,
      reply.renameTo,
    ).catch((error: unknown) => toast.error(String(error)));
  };

  return (
    <SftpConflictDialog
      key={head.prompt.conflict_id}
      prompt={head.prompt}
      onAnswer={answer}
      onDismissAsSkip={() => answer({ action: "skip", applyToAll: false })}
    />
  );
}
