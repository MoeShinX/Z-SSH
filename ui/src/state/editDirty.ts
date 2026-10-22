//! 关标签 / 退出程序时的确认文字（M3-2 的"还有 N 个传输"加上 M3-3 的"还有 M 个文件的修改没上传"）。

/** 把"进行中的传输数"和"编辑器里没保存的修改数"拼成确认框的标题和正文。 */
export function editConfirmMessage(
  transfers: number,
  dirtyEdits: number,
  action: string,
): { title: string; message: string } {
  const parts: string[] = [];
  const effects: string[] = [];
  if (transfers > 0) {
    parts.push(`${transfers} 个传输未完成`);
    effects.push("取消这些传输");
  }
  if (dirtyEdits > 0) {
    parts.push(`${dirtyEdits} 个文件的修改还没上传`);
    effects.push("丢掉这些没上传的修改");
  }
  return {
    title: `还有 ${parts.join("、")}`,
    message: `${action}会${effects.join("，并")}。`,
  };
}
