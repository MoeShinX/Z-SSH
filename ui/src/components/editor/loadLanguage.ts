//! 按需加载语法高亮包：只有打开了对应类型的文件才会下载（Vite 会把每个动态 import 切成单独的块）。

import type { Extension } from "@codemirror/state";
import type { LanguageId } from "../../state/editorLanguage";

/** 行注释用 `#` 的语言数据：Ctrl+/（切换注释）靠它知道该加什么符号。 */
export const HASH_LINE_COMMENT = { commentTokens: { line: "#" } };

async function legacy(
  load: () => Promise<Record<string, unknown>>,
  name: string,
  /** 这个模式自己没声明注释符号时补上（nginx、properties 没有，Ctrl+/ 就什么都不做）。 */
  extraLanguageData?: Record<string, unknown>,
) {
  const { StreamLanguage } = await import("@codemirror/language");
  const mode = (await load())[name] as Parameters<typeof StreamLanguage.define>[0];
  return StreamLanguage.define(
    extraLanguageData
      ? { ...mode, languageData: { ...extraLanguageData, ...(mode.languageData ?? {}) } }
      : mode,
  );
}

export async function loadLanguage(id: LanguageId | "text"): Promise<Extension | null> {
  switch (id) {
    case "shell":
      return legacy(() => import("@codemirror/legacy-modes/mode/shell"), "shell");
    case "properties":
      return legacy(
        () => import("@codemirror/legacy-modes/mode/properties"),
        "properties",
        HASH_LINE_COMMENT,
      );
    case "nginx":
      return legacy(() => import("@codemirror/legacy-modes/mode/nginx"), "nginx", HASH_LINE_COMMENT);
    case "dockerfile":
      return legacy(() => import("@codemirror/legacy-modes/mode/dockerfile"), "dockerFile");
    case "toml":
      return legacy(() => import("@codemirror/legacy-modes/mode/toml"), "toml");
    case "css":
      return legacy(() => import("@codemirror/legacy-modes/mode/css"), "css");
    case "yaml":
      return (await import("@codemirror/lang-yaml")).yaml();
    case "json":
      return (await import("@codemirror/lang-json")).json();
    case "xml":
      return (await import("@codemirror/lang-xml")).xml();
    case "html":
      return (await import("@codemirror/lang-html")).html();
    case "python":
      return (await import("@codemirror/lang-python")).python();
    case "javascript":
      return (await import("@codemirror/lang-javascript")).javascript();
    case "typescript":
      return (await import("@codemirror/lang-javascript")).javascript({ typescript: true });
    case "sql":
      return (await import("@codemirror/lang-sql")).sql();
    case "markdown":
      return (await import("@codemirror/lang-markdown")).markdown();
    default:
      return null;
  }
}
