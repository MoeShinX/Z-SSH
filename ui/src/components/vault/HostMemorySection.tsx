//! 主机详情里的「AI 记忆」：AI 用 remember_host 记下的这台主机的笔记，用户能看、能改、能清空。
//! 只存本机，不进云同步和离线备份。

import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

import { hostMemoryGet, hostMemorySet } from "../../api/vault";
import { formatStamp } from "../../state/syncModel";
import { AsidePanelSection } from "../ui/aside-panel";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";

/** 和后端 MAX_MEMORY_CHARS 一致。 */
const MAX_CHARS = 4000;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function HostMemorySection({
  hostId,
  onError,
}: {
  hostId: number;
  onError: (message: string) => void;
}) {
  const [saved, setSaved] = useState<{ content: string; updatedAt: number | null }>({
    content: "",
    updatedAt: null,
  });
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      hostMemoryGet(hostId)
        .then((memory) => {
          if (cancelled) return;
          const next = { content: memory?.content ?? "", updatedAt: memory?.updated_at ?? null };
          setSaved(next);
          setDraft(next.content);
        })
        .catch(() => {});
    void load();
    // AI 在对话里改了这台主机的笔记：跟着刷新（没在编辑时）
    let unlisten: (() => void) | null = null;
    void listen<number>("host-memory-changed", (event) => {
      if (event.payload === hostId) void load();
    })
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [hostId]);

  const save = async (content: string) => {
    setBusy(true);
    try {
      await hostMemorySet(hostId, content);
      const memory = await hostMemoryGet(hostId);
      const next = { content: memory?.content ?? "", updatedAt: memory?.updated_at ?? null };
      setSaved(next);
      setDraft(next.content);
    } catch (error) {
      onError(`保存记忆失败：${errorText(error)}`);
    } finally {
      setBusy(false);
    }
  };

  const dirty = draft !== saved.content;

  return (
    <AsidePanelSection title="AI 记忆">
      <div className="space-y-2" data-section="host-memory">
        <p className="text-xs text-muted-foreground">
          AI 排查这台主机时记下的笔记（服务、配置位置、处理过的问题、待办），下次对话会带上。
          「全部放行」模式下不带。只存在这台电脑上。
        </p>
        <Textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder="还没有记忆。和 AI 聊这台主机时，它会把有用的事实记在这里。"
          className="min-h-[120px] text-xs"
          aria-label="AI 记忆"
          maxLength={MAX_CHARS}
        />
        <div className="flex items-center gap-2">
          <span className="mr-auto text-[11px] text-muted-foreground">
            {saved.updatedAt ? `更新于 ${formatStamp(saved.updatedAt)}` : ""}
            {draft.length > 0 ? ` · ${draft.length}/${MAX_CHARS}` : ""}
          </span>
          {saved.content && (
            <Button
              variant="ghost"
              size="sm"
              className="text-destructive hover:bg-destructive/10"
              disabled={busy}
              onClick={() => void save("")}
            >
              清空
            </Button>
          )}
          <Button variant="outline" size="sm" disabled={busy || !dirty} onClick={() => void save(draft)}>
            保存记忆
          </Button>
        </div>
      </div>
    </AsidePanelSection>
  );
}
