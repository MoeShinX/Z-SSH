//! 按需加载语法高亮包：只有打开了对应类型的文件才会下载（Vite 会把每个动态 import 切成单独的块）。

import type { Extension } from "@codemirror/state";
import type { LanguageId } from "../../state/editorLanguage";

async function legacy(load: () => Promise<Record<string, unknown>>, name: string) {
  const { StreamLanguage } = await import("@codemirror/language");
  const mode = (await load())[name];
  return StreamLanguage.define(mode as Parameters<typeof StreamLanguage.define>[0]);
}

export async function loadLanguage(id: LanguageId | "text"): Promise<Extension | null> {
  switch (id) {
    case "shell":
      return legacy(() => import("@codemirror/legacy-modes/mode/shell"), "shell");
    case "properties":
      return legacy(() => import("@codemirror/legacy-modes/mode/properties"), "properties");
    case "nginx":
      return legacy(() => import("@codemirror/legacy-modes/mode/nginx"), "nginx");
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
