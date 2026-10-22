// 更新说明（取自 CHANGELOG 的一节，Markdown）→ 分组的条目，更新弹窗里按"新增 / 改进 / 修复"排版。
// 只认我们自己 CHANGELOG 用到的那点写法：`### 标题`、`- 条目`（可以续行）、`**加粗**`。
// 认不出结构（没有标题也没有条目）就返回 null，界面按原文显示。

export interface NoteItem {
  /** 条目开头的加粗部分（"**主题和皮肤**：……"里的"主题和皮肤"），没有就是空串。 */
  title: string;
  text: string;
}

export interface NoteSection {
  heading: string;
  items: NoteItem[];
}

function stripMarkdown(text: string): string {
  return text
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

function toItem(raw: string): NoteItem {
  const match = /^\*\*(.+?)\*\*\s*[：:]\s*(.*)$/s.exec(raw.trim());
  if (match) return { title: stripMarkdown(match[1]), text: stripMarkdown(match[2]) };
  return { title: "", text: stripMarkdown(raw) };
}

export function parseReleaseNotes(notes: string): NoteSection[] | null {
  const sections: NoteSection[] = [];
  let current: NoteSection | null = null;
  let item: string | null = null;
  const flush = () => {
    if (item !== null && current) current.items.push(toItem(item));
    item = null;
  };
  for (const line of notes.split(/\r?\n/)) {
    const heading = /^#{2,4}\s+(.+)$/.exec(line.trim());
    if (heading) {
      flush();
      current = { heading: stripMarkdown(heading[1]), items: [] };
      sections.push(current);
      continue;
    }
    const bullet = /^\s*[-*]\s+(.*)$/.exec(line);
    if (bullet) {
      flush();
      if (!current) {
        current = { heading: "", items: [] };
        sections.push(current);
      }
      item = bullet[1];
      continue;
    }
    if (line.trim() === "") {
      flush();
      continue;
    }
    // 条目的续行
    if (item !== null) item += ` ${line.trim()}`;
  }
  flush();
  const useful = sections.filter((section) => section.items.length > 0);
  return useful.length > 0 ? useful : null;
}

/** 分组标题的配色：新增绿、修复红、改进 / 优化蓝、安全黄，其余用默认。 */
export function sectionTone(heading: string): "added" | "fixed" | "improved" | "security" | "other" {
  if (/新增|新功能|added/i.test(heading)) return "added";
  if (/修复|fixed/i.test(heading)) return "fixed";
  if (/改进|优化|changed|improved/i.test(heading)) return "improved";
  if (/安全|security/i.test(heading)) return "security";
  return "other";
}
