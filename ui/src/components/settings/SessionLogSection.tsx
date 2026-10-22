//! 设置页"会话日志"卡片：开关、目录、打开目录。默认关。
//!
//! 日志是纯文本（去掉颜色等控制序列），每个终端会话一个文件，对新打开的会话生效。

import { useEffect, useState } from "react";

import { pickFolder } from "../../api/dialog";
import { openSessionLogDir, sessionLogDirInfo } from "../../api/settings";
import { useSettings } from "../../stores/settings";
import { Button } from "../ui/button";
import { Switch } from "../ui/switch";
import { toast } from "../ui/toast";
import { SectionCard, SettingRow } from "./settingsUi";

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function SessionLogSection() {
  const enabled = useSettings((state) => state.session_log_enabled);
  const dir = useSettings((state) => state.session_log_dir);
  const update = useSettings((state) => state.update);
  const [effectiveDir, setEffectiveDir] = useState("");

  useEffect(() => {
    let cancelled = false;
    sessionLogDirInfo()
      .then((path) => !cancelled && setEffectiveDir(path))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [dir]);

  const choose = async () => {
    const picked = await pickFolder(effectiveDir || undefined, "选择会话日志目录");
    if (picked) void update("session_log_dir", picked);
  };

  return (
    <SectionCard
      title="会话日志"
      description="把每个终端会话里显示的内容存成纯文本文件（去掉颜色和光标控制），对之后新打开的会话生效。日志里会有终端上显示过的一切，可能包括敏感信息，只存在本机。"
    >
      <SettingRow title="记录会话日志">
        <Switch
          checked={enabled}
          onCheckedChange={(checked) => void update("session_log_enabled", checked)}
          aria-label="记录会话日志"
        />
      </SettingRow>
      <SettingRow
        title="日志目录"
        description={effectiveDir ? effectiveDir : "数据目录下的 session-logs"}
      >
        <div className="flex items-center gap-2">
          {dir && (
            <Button size="sm" variant="ghost" onClick={() => void update("session_log_dir", "")}>
              恢复默认
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={() => void choose()}>
            选择…
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              void openSessionLogDir().catch((error: unknown) => toast.error(errorText(error)))
            }
          >
            打开目录
          </Button>
        </div>
      </SettingRow>
    </SectionCard>
  );
}
