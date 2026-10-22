// 系统的文件对话框（Tauri 官方 dialog 插件）。capability 只开了 dialog:allow-open 和 allow-save。
import { open, save } from "@tauri-apps/plugin-dialog";

/** 弹出系统的选择文件夹对话框；用户取消返回 null。 */
export async function pickFolder(defaultPath?: string, title?: string): Promise<string | null> {
  const picked = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath || undefined,
    title,
  });
  return typeof picked === "string" ? picked : null;
}

/** 离线备份文件的扩展名。 */
const BACKUP_FILTERS = [{ name: "Z-SSH 离线备份", extensions: ["zsshbak"] }];

/** 选一个离线备份文件来导入；取消返回 null。 */
export async function pickBackupFile(): Promise<string | null> {
  const picked = await open({ multiple: false, directory: false, filters: BACKUP_FILTERS });
  return typeof picked === "string" ? picked : null;
}

/** 选离线备份的保存位置；取消返回 null。 */
export async function pickBackupSavePath(defaultName: string): Promise<string | null> {
  return save({ defaultPath: defaultName, filters: BACKUP_FILTERS });
}
