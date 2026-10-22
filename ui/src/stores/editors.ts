//! 内置编辑器的文档仓库（M3-5 第 2 节）：每个打开的文件一份，标签、关闭询问、退出检查都读它。
//! 文本本身在 CodeMirror 里，这里只存"现在的文本怎么取"（`getText`）和未保存标记。

import { create } from "zustand";
import {
  editorClose,
  editorDraft,
  editorOpen,
  editorReload,
  editorSave,
  localEditorOpen,
  localEditorSave,
  localEditorSaveAs,
  toEditorError,
  type EditorError,
  type EditorOpenInfo,
  type EditorSaveResult,
} from "../api/editor";
import { isSameFile, tabFromInfo, type UnsavedFile } from "../state/editorModel";

export interface EditorDoc {
  editId: number;
  side: "remote" | "local";
  hostId: number;
  hostLabel: string;
  path: string;
  fileName: string;
  /** 本地文件是符号链接时，打开时解析出来的真实目标（标签悬停显示「链接 → 目标」）。 */
  linkTarget: string | null;
  bom: boolean;
  eol: "LF" | "CRLF";
  /** 打开（或放弃修改重读）时的内容；编辑器用它做初始文本。 */
  initialText: string;
  /** 重读一次就加一，编辑器据此换掉整份文本。 */
  revision: number;
  /** 内容版本：每次编辑 +1，单调递增。草稿和保存请求都带着它，保存成功后按版本删旧草稿。 */
  version: number;
  dirty: boolean;
  saving: boolean;
  /** 最近一次保存失败的原因（保存成功后清掉）。 */
  error: string | null;
  /** 最近一次交给后端的文本（保存请求里的那份）。冲突 / 属主处理完成后用它判断
   * “处理期间有没有新的输入”，不能拿编辑器现在的文本去猜。 */
  lastSentText: string | null;
  /** 编辑器挂上后注册，取当前全文。 */
  getText: (() => string) | null;
}

interface EditorsState {
  docs: Record<number, EditorDoc>;
  /** 标签栏创建 / 切换标签的钩子，由 App 注册。 */
  hooks: {
    opened: (doc: EditorDoc) => void;
    focus: (editId: number) => void;
    /** 另存为之后文档换了路径：把标签标题和悬停提示改成新的。 */
    renamed?: (editId: number, title: string) => void;
  } | null;
  setHooks: (hooks: EditorsState["hooks"]) => void;
  add: (info: EditorOpenInfo) => EditorDoc;
  patch: (editId: number, patch: Partial<EditorDoc>) => void;
  remove: (editId: number) => void;
}

/**
 * 草稿去重（PERF-2）：每个文档最近一次**写成功**的草稿请求的内容版本和文档路径。
 * 放在 store 外面，不进文档对象，写成功不会让订阅文档的组件重新渲染。
 * 版本号只增不减（每次 docChanged +1），所以“版本相同 = 内容相同”；路径要一起比，因为
 * 另存为会换草稿的归属文件名，同一版本在新路径下并不能证明草稿已经在新位置。
 */
const draftWritten = new Map<number, { version: number; path: string }>();

export const useEditors = create<EditorsState>((set, get) => ({
  docs: {},
  hooks: null,
  setHooks: (hooks) => set({ hooks }),
  add: (info) => {
    const base = tabFromInfo(info);
    const doc: EditorDoc = {
      editId: base.editId,
      side: base.side,
      hostId: base.hostId,
      hostLabel: base.hostLabel,
      path: base.path,
      fileName: base.fileName,
      linkTarget: base.linkTarget,
      bom: base.bom,
      eol: base.eol,
      initialText: info.text,
      revision: 0,
      version: 0,
      dirty: false,
      saving: false,
      error: null,
      lastSentText: null,
      getText: null,
    };
    draftWritten.delete(doc.editId);
    set({ docs: { ...get().docs, [doc.editId]: doc } });
    return doc;
  },
  patch: (editId, patch) => {
    const current = get().docs[editId];
    if (!current) return;
    set({ docs: { ...get().docs, [editId]: { ...current, ...patch } } });
  },
  remove: (editId) => {
    draftWritten.delete(editId);
    const docs = { ...get().docs };
    delete docs[editId];
    set({ docs });
  },
}));

/** 所有有未保存修改的文件。 */
export function unsavedDocs(hostId?: number): UnsavedFile[] {
  return Object.values(useEditors.getState().docs)
    .filter((doc) => doc.dirty && (hostId === undefined || (doc.side === "remote" && doc.hostId === hostId)))
    .map((doc) => ({ editId: doc.editId, fileName: doc.fileName, path: doc.path }));
}

function findExisting(side: "remote" | "local", path: string, hostId?: number): EditorDoc | null {
  const docs = Object.values(useEditors.getState().docs);
  if (side === "local") {
    return docs.find((doc) => isSameFile(doc, { side, hostId: 0, path })) ?? null;
  }
  if (hostId !== undefined) {
    return docs.find((doc) => isSameFile(doc, { side, hostId, path })) ?? null;
  }
  return docs.find((doc) => doc.side === "remote" && doc.path === path) ?? null;
}

/** 打开成功就开标签；同一个文件已经开着就切过去。失败抛 `EditorError`。 */
async function openWith(
  call: () => Promise<EditorOpenInfo>,
  side: "remote" | "local",
  path: string,
  hostId?: number,
): Promise<void> {
  const existing = findExisting(side, path, hostId);
  if (existing) {
    useEditors.getState().hooks?.focus(existing.editId);
    return;
  }
  let info: EditorOpenInfo;
  try {
    info = await call();
  } catch (error) {
    const err = toEditorError(error);
    if (err.code === "already_open") {
      const again = findExisting(side, path, hostId);
      if (again) {
        useEditors.getState().hooks?.focus(again.editId);
        return;
      }
    }
    throw err;
  }
  const doc = useEditors.getState().add(info);
  useEditors.getState().hooks?.opened(doc);
}

export function openRemoteInEditor(sftpId: string, path: string, hostId?: number): Promise<void> {
  return openWith(() => editorOpen(sftpId, path), "remote", path, hostId);
}

export function openLocalInEditor(path: string): Promise<void> {
  return openWith(() => localEditorOpen(path), "local", path);
}

/** 保存当前内容。远端要传一个现在可用的 SFTP 会话（断线时为 null：给出明确的错误，内容留在编辑器里）。 */
export async function saveDoc(
  editId: number,
  sftpId: string | null,
  force = false,
): Promise<EditorSaveResult> {
  const store = useEditors.getState();
  const doc = store.docs[editId];
  if (!doc?.getText) return { state: "failed", message: "编辑器还没有准备好" };
  if (doc.saving) return { state: "failed", message: "正在保存" };
  if (doc.side === "remote" && !sftpId) {
    const message = "这台主机的连接已经断开，内容还留在编辑器里；重新连接后再保存";
    store.patch(editId, { error: message });
    return { state: "failed", message };
  }
  const text = doc.getText();
  store.patch(editId, { saving: true, lastSentText: text });
  try {
    const result =
      doc.side === "remote"
        ? await editorSave(sftpId as string, editId, text, doc.version)
        : await localEditorSave(editId, text, force, doc.version);
    const latest = useEditors.getState().docs[editId];
    // 保存期间又改了字：仍然算未保存。
    const unchanged = latest?.getText ? latest.getText() === text : true;
    store.patch(editId, {
      saving: false,
      dirty: result.state === "uploaded" ? !unchanged : true,
      error: result.state === "uploaded" ? null : (result.message ?? "没有保存"),
    });
    return result;
  } catch (error) {
    const err: EditorError = toEditorError(error);
    store.patch(editId, { saving: false, error: err.message });
    return { state: "failed", message: err.message };
  }
}

/** 本地文件保存冲突时「另存为」：写到同目录的新名字，成功后文档改跟踪新文件。 */
export async function saveLocalAsDoc(
  editId: number,
  name: string,
): Promise<EditorSaveResult> {
  const store = useEditors.getState();
  const doc = store.docs[editId];
  if (!doc?.getText) return { state: "failed", message: "编辑器还没有准备好" };
  if (doc.side !== "local") return { state: "failed", message: "只有本地文件能这样另存为" };
  if (doc.saving) return { state: "failed", message: "正在保存" };
  const text = doc.getText();
  store.patch(editId, { saving: true, lastSentText: text });
  try {
    const result = await localEditorSaveAs(editId, text, name, doc.version);
    const latest = useEditors.getState().docs[editId];
    // 另存为期间又改了字：保存的是点按钮那一刻的内容，新字仍然算未保存。
    const unchanged = latest?.getText ? latest.getText() === text : true;
    const patch: Partial<EditorDoc> = {
      saving: false,
      dirty: result.state === "uploaded" ? !unchanged : true,
      error: result.state === "uploaded" ? null : (result.message ?? "没有保存"),
    };
    if (result.state === "uploaded" && result.path) {
      const separator = result.path.includes("\\") ? "\\" : "/";
      patch.path = result.path;
      patch.fileName = result.path.split(separator).pop() ?? latest?.fileName;
      // 另存为写出的是普通文件，不再是符号链接。
      patch.linkTarget = null;
    }
    store.patch(editId, patch);
    if (patch.fileName) store.hooks?.renamed?.(editId, patch.fileName);
    return result;
  } catch (error) {
    const err = toEditorError(error);
    store.patch(editId, { saving: false, error: err.message });
    return { state: "failed", message: err.message };
  }
}

/**
 * 草稿写盘（防崩溃）：只对有未保存修改的文件。
 *
 * - `written`：这次发了请求并写成功；
 * - `skipped`：不用写（没有未保存修改 / 编辑器没准备好 / 这个版本在这个路径下已经写成功过）；
 * - `failed`：请求失败，不记录，下个周期会重试。
 */
export async function flushDraft(editId: number): Promise<"written" | "skipped" | "failed"> {
  const doc = useEditors.getState().docs[editId];
  if (!doc?.dirty || !doc.getText) return "skipped";
  const recorded = draftWritten.get(editId);
  if (recorded && recorded.version === doc.version && recorded.path === doc.path) return "skipped";
  // 记发请求那一刻的版本和路径，不是请求完成时文档的最新状态。
  const { version, path } = doc;
  try {
    await editorDraft(editId, doc.getText(), version);
  } catch {
    return "failed";
  }
  // 请求期间文档已经关了：不能为它重新建记录。
  if (!useEditors.getState().docs[editId]) return "written";
  const current = draftWritten.get(editId);
  if (!current || current.path !== path) {
    draftWritten.set(editId, { version, path });
  } else if (version > current.version) {
    // 路径相同才取较大值：慢请求晚完成不会把记录改小。
    draftWritten.set(editId, { version, path });
  }
  return "written";
}

/** 放弃本地修改，重新读回磁盘 / 服务器上的内容。 */
export async function reloadDoc(editId: number): Promise<void> {
  const text = await editorReload(editId);
  const doc = useEditors.getState().docs[editId];
  if (!doc) return;
  useEditors.getState().patch(editId, {
    initialText: text.text,
    bom: text.bom,
    eol: text.eol,
    revision: doc.revision + 1,
    dirty: false,
    saving: false,
    error: null,
    lastSentText: null,
  });
}

/** 放弃本地修改后没能重新载入：编辑器里的文本已经过时（后端会拒绝保存它），把原因挂到文档上。 */
export function markReloadFailed(editId: number, message: string): void {
  const store = useEditors.getState();
  if (!store.docs[editId]) return;
  store.patch(editId, {
    saving: false,
    error: `已放弃本地修改，但没能重新载入服务器上的内容（${message}）。编辑器里的文字已经过时，不能保存；请关掉标签后重新打开`,
  });
}

/** 后端完成一次保存 / 冲突处理之后，把结果同步到编辑器文档（App 的 `sftp_edit_event` 里调用）。
 *
 * - `uploaded`：另存为换了路径就把文档的路径 / 文件名改过去；有没有未保存修改按**实际交给后端的
 *   那份文本**判断——处理期间用户又打了字，那些字还没传上去，仍然算未保存。
 * - `discarded`：后端已经把内容换回服务器上的版本，重新载入（内容和编码 / 换行一起）。
 */
export async function applyEditEvent(event: {
  edit_id: number;
  state: string;
  remote_path: string;
}): Promise<void> {
  const store = useEditors.getState();
  const doc = store.docs[event.edit_id];
  if (!doc) return;
  if (event.state === "discarded") {
    await reloadDoc(event.edit_id);
    return;
  }
  if (event.state !== "uploaded") return;
  const patch: Partial<EditorDoc> = { error: null, saving: false };
  if (doc.side === "remote" && event.remote_path && event.remote_path !== doc.path) {
    patch.path = event.remote_path;
    const nextName = event.remote_path.split("/").pop();
    if (nextName) patch.fileName = nextName;
  }
  const currentText = doc.getText?.() ?? null;
  if (currentText !== null && doc.lastSentText !== null) {
    // 传上去的是 lastSentText 那份；现在编辑器里不一样，就还有没保存的修改。
    patch.dirty = currentText !== doc.lastSentText;
  }
  store.patch(event.edit_id, patch);
  if (patch.fileName) store.hooks?.renamed?.(event.edit_id, patch.fileName);
}

/** 关掉一个文件；`discard` 是用户选了"不保存"。 */
export async function closeDoc(editId: number, discard: boolean): Promise<void> {
  await editorClose(editId, discard).catch(() => {});
  useEditors.getState().remove(editId);
}
