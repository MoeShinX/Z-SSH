import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/editor", async () => {
  const actual = await vi.importActual<typeof import("../api/editor")>("../api/editor");
  return {
    ...actual,
    editorOpen: vi.fn(),
    editorSave: vi.fn(),
    editorDraft: vi.fn(),
    editorReload: vi.fn(),
    editorClose: vi.fn(),
    localEditorOpen: vi.fn(),
    localEditorSave: vi.fn(),
    localEditorSaveAs: vi.fn(),
  };
});

import * as api from "../api/editor";
import {
  applyEditEvent,
  closeDoc,
  flushDraft,
  markReloadFailed,
  openLocalInEditor,
  openRemoteInEditor,
  reloadDoc,
  saveDoc,
  saveLocalAsDoc,
  unsavedDocs,
  useEditors,
} from "./editors";

const info = (over: Partial<api.EditorOpenInfo> = {}): api.EditorOpenInfo => ({
  edit_id: 1,
  side: "remote",
  text: "hello\n",
  bom: false,
  eol: "LF",
  file_name: "a.conf",
  path: "/etc/a.conf",
  host_id: 7,
  host_label: "测试主机",
  size: 6,
  ...over,
});

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.editorClose).mockResolvedValue(undefined);
  vi.mocked(api.editorDraft).mockResolvedValue(undefined);
  useEditors.setState({ docs: {}, hooks: null });
});

describe("打开", () => {
  it("成功就登记并通知标签栏", async () => {
    const opened = vi.fn();
    useEditors.getState().setHooks({ opened, focus: vi.fn() });
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    expect(opened).toHaveBeenCalledTimes(1);
    expect(useEditors.getState().docs[1].fileName).toBe("a.conf");
  });

  it("同一台主机的同一个文件再打开：切到原来的标签，不再请求后端", async () => {
    const focus = vi.fn();
    useEditors.getState().setHooks({ opened: vi.fn(), focus });
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    expect(api.editorOpen).toHaveBeenCalledTimes(1);
    expect(focus).toHaveBeenCalledWith(1);
  });

  it("另一台主机的同名路径是另一个文件", async () => {
    const opened = vi.fn();
    useEditors.getState().setHooks({ opened, focus: vi.fn() });
    vi.mocked(api.editorOpen)
      .mockResolvedValueOnce(info())
      .mockResolvedValueOnce(info({ edit_id: 2, host_id: 8, host_label: "另一台" }));
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    await openRemoteInEditor("s2", "/etc/a.conf", 8);
    expect(opened).toHaveBeenCalledTimes(2);
  });

  it("本地文件：路径大小写和斜杠不同也算同一个", async () => {
    const focus = vi.fn();
    useEditors.getState().setHooks({ opened: vi.fn(), focus });
    vi.mocked(api.localEditorOpen).mockResolvedValue(
      info({ side: "local", host_id: 0, path: "C:/Work/A.txt", file_name: "A.txt" }),
    );
    await openLocalInEditor("C:/Work/A.txt");
    await openLocalInEditor("c:\\work\\a.txt");
    expect(api.localEditorOpen).toHaveBeenCalledTimes(1);
    expect(focus).toHaveBeenCalled();
  });

  it("被拒绝（不是 UTF-8）时把结构化错误抛出去", async () => {
    vi.mocked(api.editorOpen).mockRejectedValue({ code: "not_utf8", message: "不是 UTF-8" });
    await expect(openRemoteInEditor("s1", "/b", 7)).rejects.toMatchObject({ code: "not_utf8" });
    expect(Object.keys(useEditors.getState().docs)).toHaveLength(0);
  });
});

describe("保存", () => {
  async function openOne() {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    let text = "hello\n";
    useEditors.getState().patch(1, { getText: () => text, dirty: true });
    return (next: string) => {
      text = next;
    };
  }

  it("成功后清掉未保存标记", async () => {
    await openOne();
    vi.mocked(api.editorSave).mockResolvedValue({ state: "uploaded" });
    const result = await saveDoc(1, "s1");
    expect(result.state).toBe("uploaded");
    expect(useEditors.getState().docs[1].dirty).toBe(false);
    expect(api.editorSave).toHaveBeenCalledWith("s1", 1, "hello\n", 0);
  });

  it("保存请求带着文档当前的内容版本（保存成功后后端按它删旧草稿）", async () => {
    await openOne();
    useEditors.getState().patch(1, { version: 7 });
    vi.mocked(api.editorSave).mockResolvedValue({ state: "uploaded" });
    await saveDoc(1, "s1");
    expect(api.editorSave).toHaveBeenCalledWith("s1", 1, "hello\n", 7);
  });

  it("保存的同时又改了字：仍然是未保存", async () => {
    const setText = await openOne();
    vi.mocked(api.editorSave).mockImplementation(async () => {
      setText("hello world\n");
      return { state: "uploaded" };
    });
    await saveDoc(1, "s1");
    expect(useEditors.getState().docs[1].dirty).toBe(true);
  });

  it("冲突：不清未保存标记，给出原因", async () => {
    await openOne();
    vi.mocked(api.editorSave).mockResolvedValue({ state: "conflict", message: "服务器上的文件变了" });
    await saveDoc(1, "s1");
    const doc = useEditors.getState().docs[1];
    expect(doc.dirty).toBe(true);
    expect(doc.error).toContain("变了");
  });

  it("断线时不调后端，内容保留并给出明确的错误", async () => {
    await openOne();
    const result = await saveDoc(1, null);
    expect(result.state).toBe("failed");
    expect(api.editorSave).not.toHaveBeenCalled();
    const doc = useEditors.getState().docs[1];
    expect(doc.dirty).toBe(true);
    expect(doc.error).toContain("断开");
  });

  it("后端抛错也变成界面上的错误", async () => {
    await openOne();
    vi.mocked(api.editorSave).mockRejectedValue({ code: "remote", message: "写不进去" });
    const result = await saveDoc(1, "s1");
    expect(result).toEqual({ state: "failed", message: "写不进去" });
    expect(useEditors.getState().docs[1].saving).toBe(false);
  });
});

describe("草稿、重读、关闭", () => {
  it("草稿只对有未保存修改的文件写", async () => {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    useEditors.getState().patch(1, { getText: () => "x" });
    await flushDraft(1);
    expect(api.editorDraft).not.toHaveBeenCalled();
    useEditors.getState().patch(1, { dirty: true });
    await flushDraft(1);
    expect(api.editorDraft).toHaveBeenCalledWith(1, "x", 0);
  });

  it("放弃修改重读：换掉初始文本、版本加一、未保存标记清掉", async () => {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    useEditors.getState().patch(1, { dirty: true });
    vi.mocked(api.editorReload).mockResolvedValue({ text: "new\n", bom: true, eol: "CRLF" });
    await reloadDoc(1);
    const doc = useEditors.getState().docs[1];
    expect(doc).toMatchObject({ initialText: "new\n", bom: true, eol: "CRLF", revision: 1, dirty: false });
  });

  it("放弃修改后重读失败：错误要传出来，文档保持原样并挂上不能保存的原因（R-9 T02）", async () => {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    useEditors.getState().patch(1, { dirty: true });
    vi.mocked(api.editorReload).mockRejectedValue({ code: "mixed_eol", message: "换行混用" });
    await expect(
      applyEditEvent({ edit_id: 1, state: "discarded", remote_path: "/etc/a.conf" }),
    ).rejects.toMatchObject({ code: "mixed_eol" });
    expect(useEditors.getState().docs[1]).toMatchObject({ initialText: "hello\n", revision: 0 });
    markReloadFailed(1, "换行混用");
    expect(useEditors.getState().docs[1]?.error).toContain("不能保存");
  });

  it("关闭：把「不保存」传给后端并移除", async () => {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    useEditors.getState().patch(1, { dirty: true });
    expect(unsavedDocs()).toHaveLength(1);
    expect(unsavedDocs(99)).toHaveLength(0);
    await closeDoc(1, true);
    expect(api.editorClose).toHaveBeenCalledWith(1, true);
    expect(useEditors.getState().docs[1]).toBeUndefined();
  });
});

describe("本地文件冲突的「另存为」", () => {
  function openLocalOne() {
    vi.mocked(api.localEditorOpen).mockResolvedValue(
      info({
        side: "local",
        host_id: 0,
        host_label: "本机",
        path: "C:\\work\\a.txt",
        file_name: "a.txt",
      }),
    );
    return openLocalInEditor("C:\\work\\a.txt");
  }

  it("另存为成功后文档路径 / 文件名换到实际写入的文件", async () => {
    await openLocalOne();
    useEditors.getState().patch(1, { getText: () => "my edit\n", dirty: true, version: 3 });
    vi.mocked(api.localEditorSaveAs).mockResolvedValue({
      state: "uploaded",
      path: "C:\\work\\copy.txt",
    });
    const result = await saveLocalAsDoc(1, "copy.txt");
    expect(result.state).toBe("uploaded");
    expect(api.localEditorSaveAs).toHaveBeenCalledWith(1, "my edit\n", "copy.txt", 3);
    const doc = useEditors.getState().docs[1];
    expect(doc.path).toBe("C:\\work\\copy.txt");
    expect(doc.fileName).toBe("copy.txt");
    expect(doc.dirty).toBe(false);
  });

  it("另存为期间又改了字：仍然算未保存", async () => {
    await openLocalOne();
    let text = "my edit\n";
    useEditors.getState().patch(1, { getText: () => text, dirty: true });
    vi.mocked(api.localEditorSaveAs).mockImplementation(async () => {
      text = "my edit + typing\n";
      return { state: "uploaded", path: "C:\\work\\copy.txt" };
    });
    await saveLocalAsDoc(1, "copy.txt");
    expect(useEditors.getState().docs[1].dirty).toBe(true);
  });

  it("本地符号链接：打开时记下真实目标，另存为之后不再是链接", async () => {
    vi.mocked(api.localEditorOpen).mockResolvedValue(
      info({
        side: "local",
        host_id: 0,
        host_label: "本机",
        path: "C:\\work\\link.txt",
        file_name: "link.txt",
        link_target: "C:\\data\\real.txt",
      }),
    );
    await openLocalInEditor("C:\\work\\link.txt");
    expect(useEditors.getState().docs[1].linkTarget).toBe("C:\\data\\real.txt");
    useEditors.getState().patch(1, { getText: () => "x\n", dirty: true });
    vi.mocked(api.localEditorSaveAs).mockResolvedValue({
      state: "uploaded",
      path: "C:\\work\\copy.txt",
    });
    await saveLocalAsDoc(1, "copy.txt");
    expect(useEditors.getState().docs[1].linkTarget).toBeNull();
  });
});

describe("草稿去重（同一版本成功写过就不再写）", () => {
  async function openDirty(over: Partial<api.EditorOpenInfo> = {}) {
    vi.mocked(api.editorOpen).mockResolvedValue(info(over));
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    useEditors.getState().patch(1, { getText: () => "x", dirty: true });
  }
  const setVersion = (version: number) => useEditors.getState().patch(1, { version });
  /** 手动完成的草稿请求：模拟“慢请求”。 */
  function deferredDraft() {
    let finish: () => void = () => {};
    let fail: (reason: unknown) => void = () => {};
    const promise = new Promise<void>((resolve, reject) => {
      finish = resolve;
      fail = reject;
    });
    return { promise, finish, fail };
  }

  it("同一版本写成功后，后续周期不再请求；继续输入（版本变）会写新草稿", async () => {
    await openDirty();
    expect(await flushDraft(1)).toBe("written");
    expect(await flushDraft(1)).toBe("skipped");
    expect(await flushDraft(1)).toBe("skipped");
    expect(api.editorDraft).toHaveBeenCalledTimes(1);
    setVersion(1);
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
    expect(api.editorDraft).toHaveBeenLastCalledWith(1, "x", 1);
  });

  it("写失败不记录：下次周期重试，成功后才跳过", async () => {
    await openDirty();
    vi.mocked(api.editorDraft).mockRejectedValueOnce(new Error("磁盘满"));
    expect(await flushDraft(1)).toBe("failed");
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
    expect(await flushDraft(1)).toBe("skipped");
  });

  it("慢请求：记的是发请求那一刻的版本，不是完成时的最新版本", async () => {
    await openDirty();
    const slow = deferredDraft();
    vi.mocked(api.editorDraft).mockReturnValueOnce(slow.promise);
    const pending = flushDraft(1);
    setVersion(1); // 请求在路上时又输入了
    slow.finish();
    expect(await pending).toBe("written");
    // 版本 1 的内容还没写过：必须写
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
    expect(api.editorDraft).toHaveBeenLastCalledWith(1, "x", 1);
  });

  it("慢请求晚完成：不会把已记录的较新版本改小", async () => {
    await openDirty();
    const slow = deferredDraft();
    vi.mocked(api.editorDraft).mockReturnValueOnce(slow.promise);
    const early = flushDraft(1); // 版本 0，很慢
    setVersion(1);
    expect(await flushDraft(1)).toBe("written"); // 版本 1 先完成
    slow.finish();
    await early;
    expect(await flushDraft(1)).toBe("skipped"); // 记录仍是 1
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
  });

  it("路径变了（另存为）：同一版本也要写到新位置", async () => {
    await openDirty();
    await flushDraft(1);
    useEditors.getState().patch(1, { path: "/etc/b.conf" });
    expect(await flushDraft(1)).toBe("written");
    expect(await flushDraft(1)).toBe("skipped");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
  });

  it("本地另存为之后（期间继续输入仍是未保存）：同一版本写到新位置", async () => {
    vi.mocked(api.localEditorOpen).mockResolvedValue(
      info({ side: "local", host_id: 0, path: "C:\work\a.txt", file_name: "a.txt" }),
    );
    await openLocalInEditor("C:\work\a.txt");
    let text = "v0";
    useEditors.getState().patch(1, { getText: () => text, dirty: true, version: 3 });
    await flushDraft(1);
    vi.mocked(api.localEditorSaveAs).mockImplementation(async () => {
      text = "v0 + typing";
      return { state: "uploaded", path: "C:\work\copy.txt" };
    });
    await saveLocalAsDoc(1, "copy.txt");
    expect(useEditors.getState().docs[1].dirty).toBe(true);
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
  });

  it("远程另存为（保存结果事件换了路径）之后：同一版本写到新位置", async () => {
    await openDirty();
    await flushDraft(1);
    await applyEditEvent({ edit_id: 1, state: "uploaded", remote_path: "/etc/copy.conf" });
    useEditors.getState().patch(1, { dirty: true });
    expect(useEditors.getState().docs[1].path).toBe("/etc/copy.conf");
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
  });

  it("关闭文档删记录：已关闭文档的迟到回调不会重新建记录", async () => {
    await openDirty();
    const slow = deferredDraft();
    vi.mocked(api.editorDraft).mockReturnValueOnce(slow.promise);
    const pending = flushDraft(1);
    const doc = useEditors.getState().docs[1];
    await closeDoc(1, false);
    slow.finish();
    await pending;
    // 用同一个 editId 重新放回一份文档（绕过 add）：若迟到回调留下了记录，这里会被错误地跳过
    useEditors.setState({ docs: { 1: { ...doc, dirty: true } } });
    expect(await flushDraft(1)).toBe("written");
  });

  it("关闭后重新打开同一个 editId：记录已清，第一次就写", async () => {
    await openDirty();
    await flushDraft(1);
    await closeDoc(1, false);
    await openDirty();
    expect(await flushDraft(1)).toBe("written");
    expect(api.editorDraft).toHaveBeenCalledTimes(2);
  });

  it("没有未保存修改或编辑器没准备好：不写也不记", async () => {
    vi.mocked(api.editorOpen).mockResolvedValue(info());
    await openRemoteInEditor("s1", "/etc/a.conf", 7);
    expect(await flushDraft(1)).toBe("skipped");
    useEditors.getState().patch(1, { dirty: true });
    expect(await flushDraft(1)).toBe("skipped");
    expect(api.editorDraft).not.toHaveBeenCalled();
  });
});
