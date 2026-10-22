import { describe, expect, it } from "vitest";
import type { PasteOutcome, SftpEntry } from "../api/sftp";
import {
  clipboardFrom,
  copyingText,
  pasteNotice,
  relayRetryRequest,
} from "./sftpPaste";

function entry(name: string, patch: Partial<SftpEntry> = {}): SftpEntry {
  return {
    name,
    path: `/d/${name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    ...patch,
  } as SftpEntry;
}

function outcome(patch: Partial<PasteOutcome> = {}): PasteOutcome {
  return {
    method: "server_cp",
    method_label: "服务器端复制（cp）",
    items: [{ name: "a", state: "ok" }],
    job_ids: [],
    elapsed_ms: 3400,
    ...patch,
  };
}

describe("clipboardFrom", () => {
  it("目录和指向目录的链接记成目录；非 UTF-8 名字和盘符不放进去", () => {
    const clip = clipboardFrom("remote", "s1", [
      entry("dir", { kind: "dir" }),
      entry("link", { kind: "symlink", target_kind: "dir" }),
      entry("file"),
      entry("bad", { name_is_utf8: false }),
      entry("C:", { kind: "drive" }),
    ]);
    expect(clip?.items).toEqual([
      { path: "/d/dir", is_dir: true },
      { path: "/d/link", is_dir: true },
      { path: "/d/file", is_dir: false },
    ]);
  });

  it("没有可放的条目就是 null", () => {
    expect(clipboardFrom("local", "s", [entry("x", { name_is_utf8: false })])).toBeNull();
  });
});

describe("copyingText", () => {
  it("有进度时写出第几个文件", () => {
    expect(copyingText(5)).toBe("服务器上复制中…（已 5 秒）");
    expect(copyingText(5, 2)).toBe("服务器上复制中…（第 3 个文件 · 已 5 秒）");
  });
});

describe("pasteNotice", () => {
  it("成功：写出用了哪种办法和用时", () => {
    const notice = pasteNotice(outcome());
    expect(notice.type).toBe("success");
    expect(notice.text).toContain("服务器端复制（cp）");
    expect(notice.text).toContain("3 秒");
  });

  it("失败的条目逐个写出原因，整体是错误", () => {
    const notice = pasteNotice(
      outcome({
        items: [
          { name: "a", state: "ok" },
          { name: "b", state: "failed", message: "服务器上的 cp 失败（退出码 1）：No space left" },
        ],
      }),
    );
    expect(notice.type).toBe("error");
    expect(notice.text).toContain("「b」");
    expect(notice.text).toContain("No space left");
  });

  it("进了队列的和跳过的分别说明", () => {
    const notice = pasteNotice(
      outcome({
        method: "relay",
        method_label: "经本机中转",
        items: [
          { name: "a", state: "queued" },
          { name: "b", state: "skipped" },
        ],
      }),
    );
    expect(notice.text).toContain("1 项已加入传输队列");
    expect(notice.text).toContain("跳过 1 项");
  });

  it("取消的条目明确写出来", () => {
    const notice = pasteNotice(outcome({ items: [{ name: "a", state: "cancelled" }] }));
    expect(notice.text).toContain("已取消 1 项");
    expect(notice.type).toBe("info");
  });

  it("成功的条目都写成复制，没有移动", () => {
    const notice = pasteNotice(outcome());
    expect(notice.text).toContain("已复制 1 项");
    expect(notice.text).not.toContain("移动");
  });
});

describe("relayRetryRequest", () => {
  const request = {
    paste_id: "p1",
    side: "remote" as const,
    source_sftp_id: "s1",
    items: [{ path: "/srv/a.conf", is_dir: false }],
    target_dir: "/srv",
  };

  it("只挑 cp 失败、可以中转的条目，换成 force_relay", () => {
    const retry = relayRetryRequest(
      request,
      outcome({
        items: [
          { name: "a.conf", state: "failed", path: "/srv/a.conf", is_dir: false, can_relay: true },
          { name: "b.conf", state: "failed", path: "/srv/b.conf", is_dir: false },
          { name: "c", state: "ok", path: "/srv/c", is_dir: true },
        ],
      }),
      "p2",
    );
    expect(retry).toEqual({
      ...request,
      paste_id: "p2",
      force_relay: true,
      items: [{ path: "/srv/a.conf", is_dir: false }],
    });
  });

  it("没有可中转的失败条目就是 null", () => {
    expect(relayRetryRequest(request, outcome(), "p2")).toBeNull();
    expect(
      relayRetryRequest(
        request,
        outcome({ items: [{ name: "a", state: "failed", message: "别的错" }] }),
        "p2",
      ),
    ).toBeNull();
  });
});

describe("copyingText", () => {
  it("写明在服务器上复制和已用时间", () => {
    expect(copyingText(12)).toBe("服务器上复制中…（已 12 秒）");
  });
});
