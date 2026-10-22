//! 远端重命名：先不覆盖地试一次；服务器说"已经存在"才问用户要不要覆盖（R-6b）。
//!
//! 不能只靠界面缓存的文件列表判断重名：列表可能过期（别的会话刚建了同名文件），
//! 直接传"覆盖"就会悄悄盖掉服务器上的文件。

/** 后端的"目标已存在"错误（`SftpError::AlreadyExists` 的文字）。 */
export function isAlreadyExists(error: unknown): boolean {
  return String(error).includes("已经存在");
}

export type RenameResult = "renamed" | "cancelled";

export async function renameRemoteSafely(options: {
  /** 真正去改名：`overwrite` 为 false 时目标存在必须失败。 */
  rename: (overwrite: boolean) => Promise<void>;
  /** 问用户"覆盖服务器上的同名文件？"。 */
  confirmOverwrite: () => Promise<boolean>;
}): Promise<RenameResult> {
  try {
    await options.rename(false);
    return "renamed";
  } catch (error) {
    if (!isAlreadyExists(error)) throw error;
  }
  if (!(await options.confirmOverwrite())) return "cancelled";
  await options.rename(true);
  return "renamed";
}
