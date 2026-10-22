//! 内置编辑器标签（M3-5 第 2 节）：CodeMirror 6 + 状态栏。
//! 保存走后端共享的编辑流程（冲突检测、临时文件加改名、属主、恢复记录），这里只管界面。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  EditorView,
  drawSelection,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  bracketMatching,
  defaultHighlightStyle,
  indentOnInput,
  syntaxHighlighting,
} from "@codemirror/language";
import { gotoLine, highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { oneDark } from "@codemirror/theme-one-dark";
import { cn } from "../../lib/utils";
import { useSettings, useResolvedMode } from "../../stores/settings";
import { languageFor, languageFromShebang } from "../../state/editorLanguage";
import { defaultLocalSaveAsName, statusSegments } from "../../state/editorModel";
import {
  flushDraft,
  reloadDoc,
  saveDoc,
  saveLocalAsDoc,
  useEditors,
} from "../../stores/editors";
import { toEditorError } from "../../api/editor";
import { LocalConflictDialog } from "./LocalConflictDialog";
import { loadLanguage } from "./loadLanguage";

/** 草稿写盘的间隔：有未保存修改时每隔几秒写一次，崩溃后下次启动能找回。 */
export const DRAFT_INTERVAL_MS = 4000;

interface Props {
  editId: number;
  active: boolean;
  /** 这台主机现在可用的 SFTP 会话；断线时 null。本地文件不用。 */
  sftpId: string | null;
}

export default function EditorTabView({ editId, active, sftpId }: Props) {
  const doc = useEditors((state) => state.docs[editId]);
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const sftpIdRef = useRef(sftpId);
  sftpIdRef.current = sftpId;
  const wrapCompartment = useRef(new Compartment());
  const languageCompartment = useRef(new Compartment());
  const themeCompartment = useRef(new Compartment());
  const fontCompartment = useRef(new Compartment());
  const [wrap, setWrap] = useState(false);
  const [cursor, setCursor] = useState({ line: 1, column: 1, selected: 0 });
  /** 本地文件保存冲突：弹对话框（重新载入 / 覆盖 / 另存为 / 取消）。 */
  const [localConflict, setLocalConflict] = useState<{
    suggestedName: string;
    error: string | null;
  } | null>(null);
  /** 打补丁时用最新的保存函数（Keymap 只在挂载时建一次）。 */
  const saveRef = useRef<() => void>(() => {});
  const runSave = useCallback(async () => {
    const current = useEditors.getState().docs[editId];
    const result = await saveDoc(editId, sftpIdRef.current);
    // 远端冲突由 App 的 sftp_edit_event 弹对话框；本地冲突在这里弹。
    if (result.state === "conflict" && current?.side === "local") {
      setLocalConflict({ suggestedName: defaultLocalSaveAsName(current.fileName), error: null });
    }
  }, [editId]);
  saveRef.current = () => void runSave();

  const uiMode = useResolvedMode();
  const fontFamily = useSettings((state) => state.terminal_font_family);
  const fontSize = useSettings((state) => state.terminal_font_size);

  const language = useMemo(() => (doc ? languageFor(doc.path) : "text"), [doc?.path]);
  const [effectiveLanguage, setEffectiveLanguage] = useState(language);

  // 创建编辑器（只在这个文件第一次出现时）。
  useEffect(() => {
    const current = useEditors.getState().docs[editId];
    if (!current || !hostRef.current) return;
    const updateListener = EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        // 内容版本单调递增：草稿和保存请求都带着它，保存成功后只删版本不晚于它的草稿。
        const current = useEditors.getState().docs[editId];
        useEditors.getState().patch(editId, {
          dirty: true,
          version: (current?.version ?? 0) + 1,
        });
      }
      if (update.selectionSet || update.docChanged) {
        const head = update.state.selection.main.head;
        const line = update.state.doc.lineAt(head);
        const range = update.state.selection.main;
        setCursor({
          line: line.number,
          column: head - line.from + 1,
          selected: Math.abs(range.to - range.from),
        });
      }
    });
    const state = EditorState.create({
      doc: current.initialText,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightActiveLine(),
        drawSelection(),
        history(),
        indentOnInput(),
        bracketMatching(),
        highlightSelectionMatches(),
        search({ top: true }),
        keymap.of([
          {
            key: "Mod-s",
            preventDefault: true,
            run: () => {
              saveRef.current();
              return true;
            },
          },
          { key: "Mod-g", run: gotoLine, preventDefault: true },
          ...searchKeymap,
          ...historyKeymap,
          indentWithTab,
          ...defaultKeymap,
        ]),
        updateListener,
        wrapCompartment.current.of([]),
        languageCompartment.current.of([]),
        themeCompartment.current.of([]),
        fontCompartment.current.of([]),
      ],
    });
    const view = new EditorView({ state, parent: hostRef.current });
    viewRef.current = view;
    useEditors.getState().patch(editId, { getText: () => view.state.doc.toString() });
    return () => {
      useEditors.getState().patch(editId, { getText: null });
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editId]);

  // 放弃修改重读后，整份文本换掉。
  const revision = doc?.revision ?? 0;
  const lastRevision = useRef(0);
  useEffect(() => {
    const view = viewRef.current;
    const current = useEditors.getState().docs[editId];
    if (!view || !current || revision === lastRevision.current) return;
    lastRevision.current = revision;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: current.initialText },
    });
    useEditors.getState().patch(editId, { dirty: false });
  }, [editId, revision]);

  // 语言：按文件名，认不出再看第一行的 shebang。
  useEffect(() => {
    let cancelled = false;
    const view = viewRef.current;
    if (!view) return;
    let id = language;
    if (id === "text") {
      const first = view.state.doc.line(1).text;
      id = languageFromShebang(first) ?? "text";
    }
    setEffectiveLanguage(id);
    void loadLanguage(id)
      .then((extension) => {
        if (cancelled || !viewRef.current) return;
        viewRef.current.dispatch({
          effects: languageCompartment.current.reconfigure(extension ?? []),
        });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [language, editId]);

  // 主题跟界面主题，字体跟终端设置。
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const dark = uiMode === "dark";
    const base: Extension = EditorView.theme(
      {
        "&": {
          height: "100%",
          backgroundColor: "hsl(var(--background))",
          color: "hsl(var(--foreground))",
        },
        ".cm-gutters": {
          backgroundColor: "hsl(var(--card))",
          color: "hsl(var(--muted-foreground))",
          border: "none",
        },
        ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "hsl(var(--foreground) / 0.06)" },
        ".cm-cursor": { borderLeftColor: "hsl(var(--foreground))" },
        ".cm-panels": {
          backgroundColor: "hsl(var(--card))",
          color: "hsl(var(--foreground))",
        },
      },
      { dark },
    );
    view.dispatch({
      effects: themeCompartment.current.reconfigure([
        base,
        dark ? oneDark : syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        // oneDark 自带背景色，放在后面把背景改回界面主题的。
        EditorView.theme({ "&": { backgroundColor: "hsl(var(--background))" } }, { dark }),
      ]),
    });
  }, [uiMode, editId]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: fontCompartment.current.reconfigure(
        EditorView.theme({
          ".cm-scroller": { fontFamily, fontSize: `${fontSize}px` },
        }),
      ),
    });
  }, [fontFamily, fontSize, editId]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: wrapCompartment.current.reconfigure(wrap ? EditorView.lineWrapping : []),
    });
  }, [wrap, editId]);

  // 草稿：有未保存修改时定时写盘。
  const dirty = doc?.dirty ?? false;
  useEffect(() => {
    if (!dirty) return;
    const timer = window.setInterval(() => void flushDraft(editId), DRAFT_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [dirty, editId]);

  // 切回这个标签时把焦点还给编辑器。
  useEffect(() => {
    if (active) viewRef.current?.focus();
  }, [active]);

  if (!doc) return null;
  const segments = statusSegments({
    bom: doc.bom,
    eol: doc.eol,
    line: cursor.line,
    column: cursor.column,
    language: effectiveLanguage,
    wrap,
    selected: cursor.selected,
  });
  const disconnected = doc.side === "remote" && !sftpId;

  /** 本地冲突：重新载入 = 丢掉我的修改，读磁盘上现在的版本。 */
  const handleLocalReload = () => {
    setLocalConflict(null);
    void reloadDoc(editId).catch((error: unknown) => {
      const err = toEditorError(error);
      useEditors.getState().patch(editId, { error: err.message });
    });
  };
  /** 本地冲突：覆盖 = 用户确认这次冲突，force=true 再写一次。 */
  const handleLocalOverwrite = () => {
    setLocalConflict(null);
    void saveDoc(editId, sftpIdRef.current, true);
  };
  /** 本地冲突：另存为同目录下的新名字，成功之后文档改跟踪新文件。 */
  const handleLocalSaveAs = (name: string) => {
    void saveLocalAsDoc(editId, name).then(
      (result) => {
        if (result.state === "uploaded") {
          setLocalConflict(null);
        } else {
          setLocalConflict((current) =>
            current ? { ...current, error: result.message ?? "没有保存" } : current,
          );
        }
      },
      (error: unknown) => {
        const err = toEditorError(error);
        setLocalConflict((current) =>
          current ? { ...current, error: err.message } : current,
        );
      },
    );
  };

  return (
    <div className="flex h-full min-h-0 flex-col" data-editor-id={editId}>
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-border px-3 text-xs">
        <span className="truncate text-muted-foreground" title={doc.path}>
          {doc.side === "remote" ? `${doc.hostLabel}:${doc.path}` : doc.path}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            data-action="editor-wrap"
            aria-pressed={wrap}
            onClick={() => setWrap((value) => !value)}
            className={cn(
              "rounded px-2 py-0.5 hover:bg-foreground/10",
              wrap && "bg-foreground/10 text-foreground",
            )}
          >
            自动换行
          </button>
          <button
            type="button"
            data-action="editor-save"
            disabled={doc.saving}
            onClick={() => saveRef.current()}
            className="rounded bg-primary px-2 py-0.5 text-primary-foreground hover:opacity-90 disabled:opacity-50"
          >
            {doc.saving ? "保存中…" : "保存 (Ctrl+S)"}
          </button>
        </div>
      </div>
      {(doc.error || disconnected) && (
        <div
          role="alert"
          data-testid="editor-error"
          className="shrink-0 border-b border-border bg-rose-500/10 px-3 py-1 text-xs text-rose-400"
        >
          {doc.error ?? "这台主机的连接已经断开，内容还留在编辑器里；重新连接后再保存"}
        </div>
      )}
      <div ref={hostRef} className="min-h-0 flex-1 overflow-hidden" />
      <div
        data-testid="editor-status"
        className="flex h-6 shrink-0 items-center gap-4 border-t border-border px-3 text-xs text-muted-foreground"
      >
        {segments.map((segment, index) => (
          <span key={index}>{segment}</span>
        ))}
        {doc.dirty && <span className="ml-auto text-amber-400">未保存</span>}
      </div>
      {localConflict && (
        <LocalConflictDialog
          fileName={doc.fileName}
          suggestedName={localConflict.suggestedName}
          error={localConflict.error}
          onReload={handleLocalReload}
          onOverwrite={handleLocalOverwrite}
          onSaveAs={handleLocalSaveAs}
          onCancel={() => setLocalConflict(null)}
        />
      )}
    </div>
  );
}
