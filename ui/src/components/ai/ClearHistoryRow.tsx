//! 设置页 AI 区的"清空 AI 历史"（R11）。点一次先要确认，再点才删。

import { useState } from "react";

import { aiHistoryClear } from "../../api/agent";
import { Button } from "../ui/button";
import { toast } from "../ui/toast";
import { SettingRow } from "../settings/settingsUi";

export function ClearAiHistoryRow() {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const clear = () => {
    setBusy(true);
    void aiHistoryClear()
      .then((removed) => toast.success(`已清空 AI 历史（${removed} 个会话）`))
      .catch((error: unknown) => toast.error(`清空 AI 历史失败：${String(error)}`))
      .finally(() => {
        setBusy(false);
        setConfirming(false);
      });
  };

  return (
    <SettingRow
      title="AI 历史"
      description="AI 面板的历史会话保存在本机数据目录里（每种智能体最多 50 个），是未加密的明文，包括命令输出的摘要（每条最多 4 KB）——如果让 AI 看过 .env、密钥之类的文件，内容可能在里面。不需要时可以清空，清空后不能恢复。"
    >
      {confirming ? (
        <div className="flex gap-2" data-section="clear-ai-history-confirm">
          <Button size="sm" variant="outline" disabled={busy} onClick={() => setConfirming(false)}>
            取消
          </Button>
          <Button size="sm" variant="destructive" disabled={busy} onClick={clear}>
            确认清空
          </Button>
        </div>
      ) : (
        <Button
          size="sm"
          variant="outline"
          data-action="clear-ai-history"
          onClick={() => setConfirming(true)}
        >
          清空 AI 历史
        </Button>
      )}
    </SettingRow>
  );
}
