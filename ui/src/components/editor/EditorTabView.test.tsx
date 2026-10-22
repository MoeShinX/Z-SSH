/** @vitest-environment jsdom */
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/editor", async () => {
  const actual = await vi.importActual<typeof import("../../api/editor")>("../../api/editor");
  return {
    ...actual,
    editorSave: vi.fn(),
    editorDraft: vi.fn(),
    editorReload: vi.fn(),
    editorClose: vi.fn(),
    localEditorOpen: vi.fn(),
    localEditorSave: vi.fn(),
    localEditorSaveAs: vi.fn(),
  };
});

import { EditorView } from "@codemirror/view";
import * as api from "../../api/editor";
import { applyEditEvent, useEditors } from "../../stores/editors";
import EditorTabView, { DRAFT_INTERVAL_MS } from "./EditorTabView";

// CodeMirror 量字符尺寸要用到这两个，jsdom 没有。
beforeEach(() => {
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
  vi.resetAllMocks();
  vi.mocked(api.editorSave).mockResolvedValue({ state: "uploaded" });
  vi.mocked(api.editorDraft).mockResolvedValue(undefined);
  useEditors.setState({ docs: {}, hooks: null });
  useEditors.getState().add({
    edit_id: 5,
    side: "remote",
    text: "server {\n  listen 80;\n}\n",
    bom: true,
    eol: "CRLF",
    file_name: "site.conf",
    path: "/etc/nginx/conf.d/site.conf",
    host_id: 3,
    host_label: "测试主机",
    size: 24,
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function typeInto(text: string) {
  // 直接改 CodeMirror 的文档（jsdom 里没有真实的键盘输入）。
  const view = EditorView.findFromDOM(document.querySelector(".cm-editor") as HTMLElement);
  view?.dispatch({ changes: { from: 0, insert: text } });
}

describe("EditorTabView", () => {
  it("显示内容、文件位置和状态栏（编码、换行、语言）", async () => {
    render(<EditorTabView editId={5} active sftpId="s1" />);
    expect(document.querySelector(".cm-content")?.textContent).toContain("listen 80;");
    const status = screen.getByTestId("editor-status").textContent ?? "";
    expect(status).toContain("UTF-8 (BOM)");
    expect(status).toContain("CRLF");
    expect(screen.getByText("测试主机:/etc/nginx/conf.d/site.conf")).toBeTruthy();
    await waitFor(() => expect(screen.getByTestId("editor-status").textContent).toContain("Nginx"));
  });

  it("点保存按钮 → 带着全文和当前内容版本调后端", async () => {
    render(<EditorTabView editId={5} active sftpId="s1" />);
    fireEvent.click(document.querySelector('[data-action="editor-save"]') as HTMLElement);
    await waitFor(() =>
      expect(api.editorSave).toHaveBeenCalledWith("s1", 5, "server {\n  listen 80;\n}\n", 0),
    );
  });

  it("断线：提示明确，保存不调后端", async () => {
    render(<EditorTabView editId={5} active sftpId={null} />);
    expect(screen.getByTestId("editor-error").textContent).toContain("断开");
    fireEvent.click(document.querySelector('[data-action="editor-save"]') as HTMLElement);
    await waitFor(() => expect(useEditors.getState().docs[5].error).toContain("断开"));
    expect(api.editorSave).not.toHaveBeenCalled();
  });

  it("有未保存修改时定时写草稿，没有修改不写", async () => {
    vi.useFakeTimers();
    render(<EditorTabView editId={5} active sftpId="s1" />);
    await act(async () => {
      vi.advanceTimersByTime(DRAFT_INTERVAL_MS * 2);
    });
    expect(api.editorDraft).not.toHaveBeenCalled();
    await act(async () => {
      useEditors.getState().patch(5, { dirty: true });
    });
    await act(async () => {
      vi.advanceTimersByTime(DRAFT_INTERVAL_MS + 10);
    });
    expect(api.editorDraft).toHaveBeenCalledWith(5, "server {\n  listen 80;\n}\n", 0);
  });

  it("同一版本草稿写成功后，后续周期不再请求；继续输入再写；失败下个周期重试", async () => {
    vi.useFakeTimers();
    render(<EditorTabView editId={5} active sftpId="s1" />);
    await act(async () => {
      useEditors.getState().patch(5, { dirty: true });
    });
    for (let i = 0; i < 3; i += 1) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(DRAFT_INTERVAL_MS);
      });
    }
    expect(api.editorDraft).toHaveBeenCalledTimes(1);

    await act(async () => {
      typeInto("# x\n");
    });
    vi.mocked(api.editorDraft).mockRejectedValueOnce(new Error("写不了"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DRAFT_INTERVAL_MS);
    });
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DRAFT_INTERVAL_MS);
    });
    expect(api.editorDraft).toHaveBeenCalledTimes(3);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DRAFT_INTERVAL_MS * 2);
    });
    expect(api.editorDraft).toHaveBeenCalledTimes(3);
    expect(api.editorDraft).toHaveBeenLastCalledWith(5, expect.stringContaining("# x"), 1);
  });

  it("编辑后标成未保存", async () => {
    render(<EditorTabView editId={5} active sftpId="s1" />);
    typeInto("# x\n");
    await waitFor(() => expect(useEditors.getState().docs[5].dirty).toBe(true));
    expect(screen.getByTestId("editor-status").textContent).toContain("未保存");
  });

  it("每次编辑内容版本单调递增；保存请求带着编辑后的版本（草稿按版本删）", async () => {
    render(<EditorTabView editId={5} active sftpId="s1" />);
    typeInto("# x\n");
    await waitFor(() => expect(useEditors.getState().docs[5].version).toBe(1));
    typeInto("# y\n");
    await waitFor(() => expect(useEditors.getState().docs[5].version).toBe(2));
    fireEvent.click(document.querySelector('[data-action="editor-save"]') as HTMLElement);
    await waitFor(() =>
      expect(api.editorSave).toHaveBeenCalledWith("s1", 5, expect.any(String), 2),
    );
  });
});

// ---------------------------------------------------------------------------
// 第 7 节：本地文件保存遇到冲突时的出路（重新载入 / 覆盖 / 另存为 / 取消）
// ---------------------------------------------------------------------------

const LOCAL_ID = 6;

function seedLocalDoc() {
  useEditors.setState({ docs: {}, hooks: null });
  useEditors.getState().add({
    edit_id: LOCAL_ID,
    side: "local",
    text: "disk content\n",
    bom: false,
    eol: "LF",
    file_name: "a.txt",
    path: "C:\\work\\a.txt",
    host_id: 0,
    host_label: "本机",
    size: 13,
  });
  // 保存过一版，所以文档有“交给后端的那份文本”。
  useEditors.getState().patch(LOCAL_ID, { lastSentText: "disk content\n" });
}

async function clickSaveLocal() {
  await act(async () => {
    fireEvent.click(document.querySelector('[data-action="editor-save"]') as HTMLElement);
  });
}

describe("本地文件保存冲突（第 7 节）", () => {
  beforeEach(() => {
    vi.mocked(api.localEditorSave).mockResolvedValue({
      state: "conflict",
      message: "这个文件在你打开之后被别的程序改过，没有保存",
    });
  });

  it("冲突时弹对话框；点「覆盖」才带 force=true 再保存", async () => {
    seedLocalDoc();
    vi.mocked(api.localEditorSave)
      .mockResolvedValueOnce({ state: "conflict", message: "文件被别的程序改过" })
      .mockResolvedValueOnce({ state: "uploaded" });
    render(<EditorTabView editId={LOCAL_ID} active sftpId={null} />);
    await clickSaveLocal();

    expect(await screen.findByText("重新载入（丢掉我的修改）")).toBeTruthy();
    expect(screen.getByText("覆盖")).toBeTruthy();

    fireEvent.click(screen.getByText("覆盖"));
    await waitFor(() =>
      expect(api.localEditorSave).toHaveBeenLastCalledWith(
        LOCAL_ID,
        expect.any(String),
        true,
        expect.any(Number),
      ),
    );
    // 覆盖成功之后对话框收起。
    await waitFor(() => expect(screen.queryByText("重新载入（丢掉我的修改）")).toBeNull());
  });

  it("点「重新载入」：重新读磁盘上的内容（丢掉我的修改）", async () => {
    seedLocalDoc();
    vi.mocked(api.editorReload).mockResolvedValue({ text: "changed by another program\n", bom: false, eol: "LF" });
    render(<EditorTabView editId={LOCAL_ID} active sftpId={null} />);
    // 先让编辑器里有“我的修改”。
    await act(async () => {
      const view = EditorView.findFromDOM(document.querySelector(".cm-editor") as HTMLElement);
      view?.dispatch({ changes: { from: 0, insert: "my edit\n" } });
    });
    await clickSaveLocal();
    fireEvent.click(await screen.findByText("重新载入（丢掉我的修改）"));

    await waitFor(() => expect(api.editorReload).toHaveBeenCalledWith(LOCAL_ID));
    await waitFor(() =>
      expect(document.querySelector(".cm-content")?.textContent).toContain(
        "changed by another program",
      ),
    );
    expect(useEditors.getState().docs[LOCAL_ID].dirty).toBe(false);
  });

  it("点「另存为」：带新名字存一份，文档路径跟着实际写入的文件走", async () => {
    seedLocalDoc();
    vi.mocked(api.localEditorSaveAs).mockResolvedValue({
      state: "uploaded",
      path: "C:\\work\\copy.txt",
    });
    render(<EditorTabView editId={LOCAL_ID} active sftpId={null} />);
    await clickSaveLocal();

    const input = (await screen.findByText("另存为（同一目录下的新文件）")).parentElement
      ?.querySelector("input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "copy.txt" } });
    fireEvent.click(screen.getByText("另存为"));

    await waitFor(() =>
      expect(api.localEditorSaveAs).toHaveBeenCalledWith(
        LOCAL_ID,
        expect.any(String),
        "copy.txt",
        expect.any(Number),
      ),
    );
    await waitFor(() => {
      const doc = useEditors.getState().docs[LOCAL_ID];
      expect(doc.path).toBe("C:\\work\\copy.txt");
      expect(doc.fileName).toBe("copy.txt");
    });
    expect(screen.queryByText("重新载入（丢掉我的修改）")).toBeNull();
  });

  it("点「取消」：对话框收起，不调任何保存 / 重读命令", async () => {
    seedLocalDoc();
    render(<EditorTabView editId={LOCAL_ID} active sftpId={null} />);
    await clickSaveLocal();
    await screen.findByText("重新载入（丢掉我的修改）");
    fireEvent.click(screen.getByText("取消"));
    await waitFor(() => expect(screen.queryByText("重新载入（丢掉我的修改）")).toBeNull());
    expect(api.localEditorSave).toHaveBeenCalledTimes(1);
    expect(api.localEditorSaveAs).not.toHaveBeenCalled();
    expect(api.editorReload).not.toHaveBeenCalled();
  });
});

// ---- AUDIT-2 A04：重新载入期间输入的字不能被吞掉 ----

type ReloadPayload = { text: string; bom: boolean; eol: "LF" | "CRLF" };

/** 把 editorReload 换成"手动放行"的：测试决定什么时候返回、返回什么、或者失败。 */
function deferReload() {
  let resolve!: (value: ReloadPayload) => void;
  let reject!: (reason: unknown) => void;
  vi.mocked(api.editorReload).mockImplementation(
    () =>
      new Promise<ReloadPayload>((res, rej) => {
        resolve = res;
        reject = rej;
      }),
  );
  return {
    resolve: (text: string) => resolve({ text, bom: false, eol: "LF" }),
    reject: (message: string) => reject({ code: "io", message }),
  };
}

function editorText(): string {
  return useEditors.getState().docs[LOCAL_ID].getText?.() ?? "";
}

/** 用户的输入：直接往 CodeMirror 派发改动（jsdom 里没有真实键盘，和其他用例一样）。 */
function userTypes(text: string) {
  const view = EditorView.findFromDOM(document.querySelector(".cm-editor") as HTMLElement);
  view?.dispatch({ changes: { from: 0, insert: text } });
}

/** 打开本地文档、先打一行字、触发保存冲突、点「重新载入」，对话框收起后返回（reload 还没放行）。 */
async function startLocalReload() {
  seedLocalDoc();
  vi.mocked(api.localEditorSave).mockResolvedValue({
    state: "conflict",
    message: "这个文件在你打开之后被别的程序改过，没有保存",
  });
  const reload = deferReload();
  render(<EditorTabView editId={LOCAL_ID} active sftpId={null} />);
  await act(async () => userTypes("my edit\n"));
  await clickSaveLocal();
  fireEvent.click(await screen.findByText("重新载入（丢掉我的修改）"));
  await waitFor(() => expect(screen.queryByText("重新载入（丢掉我的修改）")).toBeNull());
  await waitFor(() => expect(api.editorReload).toHaveBeenCalledWith(LOCAL_ID));
  return reload;
}

describe("重新载入期间的输入（AUDIT-2 A04）", () => {
  it("对照：载入期间不输入，载入完成显示磁盘内容并清掉未保存标记", async () => {
    const reload = await startLocalReload();
    await act(async () => reload.resolve("disk now\n"));
    expect(editorText()).toBe("disk now\n");
    const doc = useEditors.getState().docs[LOCAL_ID];
    expect(doc).toMatchObject({ dirty: false, reloading: false, error: null });
  });

  it("载入期间编辑器只读：输入被挡住，保存按钮不可用，载入完成后恢复可编辑", async () => {
    const reload = await startLocalReload();
    expect(useEditors.getState().docs[LOCAL_ID].reloading).toBe(true);
    expect(screen.getByTestId("editor-reloading")).toBeTruthy();
    expect(
      (document.querySelector('[data-action="editor-save"]') as HTMLButtonElement).disabled,
    ).toBe(true);
    const before = useEditors.getState().docs[LOCAL_ID].version;

    await act(async () => userTypes("NEW_INPUT_WHILE_LOADING:"));
    expect(editorText()).not.toContain("NEW_INPUT_WHILE_LOADING:");
    expect(useEditors.getState().docs[LOCAL_ID].version).toBe(before);

    await act(async () => reload.resolve("disk now\n"));
    expect(editorText()).toBe("disk now\n");
    expect(useEditors.getState().docs[LOCAL_ID]).toMatchObject({ dirty: false, reloading: false });
    expect(screen.queryByTestId("editor-reloading")).toBeNull();

    // 载入结束后又能正常输入，而且算未保存修改。
    await act(async () => userTypes("after:"));
    expect(editorText()).toBe("after:disk now\n");
    expect(useEditors.getState().docs[LOCAL_ID].dirty).toBe(true);
  });

  it("载入完成后内容版本往前走一格，草稿 / 保存不会把新内容当成旧内容", async () => {
    const reload = await startLocalReload();
    const before = useEditors.getState().docs[LOCAL_ID].version;
    await act(async () => reload.resolve("disk now\n"));
    expect(useEditors.getState().docs[LOCAL_ID].version).toBe(before + 1);
  });

  it("版本在载入期间变了（还是有改动漏进来）：不覆盖，保留文本和未保存标记，并给出提示", async () => {
    const reload = await startLocalReload();
    // 模拟只读之外漏进来的一次改动：内容版本往前走了。
    await act(async () => {
      const doc = useEditors.getState().docs[LOCAL_ID];
      useEditors.getState().patch(LOCAL_ID, { version: doc.version + 1, dirty: true });
    });
    await act(async () => reload.resolve("disk now\n"));
    expect(editorText()).toContain("my edit");
    expect(editorText()).not.toContain("disk now");
    const doc = useEditors.getState().docs[LOCAL_ID];
    expect(doc).toMatchObject({ dirty: true, reloading: false, revision: 0 });
    expect(doc.error).toContain("重新载入期间内容有改动，没有覆盖");
    expect(screen.getByTestId("editor-error").textContent).toContain("没有覆盖");
  });

  it("载入失败：保留原文和未保存标记，恢复可编辑", async () => {
    const reload = await startLocalReload();
    await act(async () => reload.reject("读不了"));
    expect(editorText()).toContain("my edit");
    await waitFor(() =>
      expect(useEditors.getState().docs[LOCAL_ID]).toMatchObject({
        dirty: true,
        reloading: false,
        error: "读不了",
      }),
    );
    await act(async () => userTypes("again:"));
    expect(editorText()).toContain("again:");
  });

  it("远端冲突事件（discarded）那条路径同样：期间只读，载入后显示服务器内容", async () => {
    const reload = deferReload();
    render(<EditorTabView editId={5} active sftpId="s1" />);
    await act(async () => typeInto("mine:"));
    const pending = applyEditEvent({
      edit_id: 5,
      state: "discarded",
      remote_path: "/etc/nginx/conf.d/site.conf",
    });
    await waitFor(() => expect(useEditors.getState().docs[5].reloading).toBe(true));
    await act(async () => typeInto("LATE:"));
    expect(useEditors.getState().docs[5].getText?.()).not.toContain("LATE:");
    await act(async () => {
      reload.resolve("from server\n");
      await expect(pending).resolves.toBe("reloaded");
    });
    expect(useEditors.getState().docs[5].getText?.()).toBe("from server\n");
    expect(useEditors.getState().docs[5]).toMatchObject({ dirty: false, reloading: false });
  });

  it("远端冲突事件那条路径：版本变了不覆盖，结果是 kept", async () => {
    const reload = deferReload();
    render(<EditorTabView editId={5} active sftpId="s1" />);
    await act(async () => typeInto("mine:"));
    const pending = applyEditEvent({
      edit_id: 5,
      state: "discarded",
      remote_path: "/etc/nginx/conf.d/site.conf",
    });
    await waitFor(() => expect(useEditors.getState().docs[5].reloading).toBe(true));
    await act(async () => {
      const doc = useEditors.getState().docs[5];
      useEditors.getState().patch(5, { version: doc.version + 1 });
    });
    await act(async () => {
      reload.resolve("from server\n");
      await expect(pending).resolves.toBe("kept");
    });
    expect(useEditors.getState().docs[5].getText?.()).toContain("mine:");
    expect(useEditors.getState().docs[5]).toMatchObject({ dirty: true, reloading: false });
    expect(useEditors.getState().docs[5].error).toContain("没有覆盖");
  });
});
