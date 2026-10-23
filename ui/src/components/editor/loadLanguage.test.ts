/** @vitest-environment jsdom */
//! Ctrl+/（切换行注释）要知道注释符号：nginx、properties（.conf / .ini / sshd_config）这两个旧模式
//! 自己没声明，以前按了什么都不发生；纯文本用编辑器补的 `#`。

import { toggleComment } from "@codemirror/commands";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { describe, expect, it } from "vitest";

import { HASH_LINE_COMMENT, loadLanguage } from "./loadLanguage";

function toggled(text: string, language: Extension): string {
  const view = new EditorView({
    state: EditorState.create({ doc: text, extensions: [language] }),
    parent: document.body,
  });
  toggleComment(view);
  const result = view.state.doc.toString();
  view.destroy();
  return result;
}

describe("Ctrl+/ 切换注释", () => {
  it.each(["nginx", "properties", "shell", "toml", "dockerfile"] as const)("%s 用 # 注释", async (id) => {
    const language = await loadLanguage(id);
    expect(toggled("listen 80;", language!)).toBe("# listen 80;");
    expect(toggled("# listen 80;", language!)).toBe("listen 80;");
  });

  it("纯文本用编辑器补的 #", () => {
    const plain = EditorState.languageData.of(() => [HASH_LINE_COMMENT]);
    expect(toggled("PermitRootLogin no", plain)).toBe("# PermitRootLogin no");
  });

  it("语言包自带的注释符号不被覆盖（JavaScript 用 //）", async () => {
    const js = await loadLanguage("javascript");
    expect(toggled("let a = 1;", js!)).toBe("// let a = 1;");
  });
});
