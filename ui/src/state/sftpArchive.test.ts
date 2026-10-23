import { describe, expect, it } from "vitest";

import type { ArchiveCaps, ExtractPlan, SftpEntry } from "../api/sftp";
import {
  archiveBusyText,
  archiveFormatOf,
  compressNotice,
  compressToolReason,
  conflictMoreText,
  extractFolderName,
  extractNotice,
  extractToolReason,
  isExtractable,
} from "./sftpArchive";

function entry(overrides: Partial<SftpEntry> & { name: string }): SftpEntry {
  return {
    path: `/srv/${overrides.name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    ...overrides,
  };
}

describe("压缩包后缀", () => {
  it("认得的八种后缀，不分大小写", () => {
    expect(archiveFormatOf("a.tar.gz")).toBe("tar_gz");
    expect(archiveFormatOf("a.tgz")).toBe("tar_gz");
    expect(archiveFormatOf("a.TAR.GZ")).toBe("tar_gz");
    expect(archiveFormatOf("a.tar")).toBe("tar");
    expect(archiveFormatOf("a.tar.xz")).toBe("tar_xz");
    expect(archiveFormatOf("a.txz")).toBe("tar_xz");
    expect(archiveFormatOf("a.tar.bz2")).toBe("tar_bz2");
    expect(archiveFormatOf("a.tbz2")).toBe("tar_bz2");
    expect(archiveFormatOf("备份 2026.ZIP")).toBe("zip");
  });

  it("认不得的：别的压缩格式、只有后缀没有主名、后缀后面还有东西", () => {
    for (const name of ["a.gz", "a.rar", "a.7z", "zip", ".zip", ".tar.gz", "a.tar.gz.bak", "a.tgzz"]) {
      expect(archiveFormatOf(name), name).toBeNull();
    }
  });

  it("同名文件夹名是去掉后缀后的名字", () => {
    expect(extractFolderName("site.tar.gz")).toBe("site");
    expect(extractFolderName("site.tgz")).toBe("site");
    expect(extractFolderName("my site.ZIP")).toBe("my site");
    expect(extractFolderName("a.b.tar.xz")).toBe("a.b");
    expect(extractFolderName("中文包.tar")).toBe("中文包");
    expect(extractFolderName("..zip")).toBeNull();
    expect(extractFolderName("readme.txt")).toBeNull();
  });

  it("能解压的条目：文件或指向文件的链接、名字合法、后缀认得", () => {
    expect(isExtractable(entry({ name: "a.zip" }))).toBe(true);
    expect(isExtractable(entry({ name: "a.zip", kind: "symlink", target_kind: "file" }))).toBe(true);
    expect(isExtractable(entry({ name: "a.zip", kind: "dir" }))).toBe(false);
    expect(isExtractable(entry({ name: "a.zip", name_is_utf8: false }))).toBe(false);
    expect(isExtractable(entry({ name: "a.txt" }))).toBe(false);
  });
});

describe("服务器上缺什么命令", () => {
  const caps = (patch: Partial<ArchiveCaps>): ArchiveCaps => ({
    posix: true,
    tar: true,
    zip: true,
    unzip: true,
    ...patch,
  });

  it("压缩", () => {
    expect(compressToolReason("tar_gz", caps({}))).toBeNull();
    expect(compressToolReason("tar_gz", caps({ tar: false }))).toBe("服务器上没有 tar 命令");
    expect(compressToolReason("zip", caps({ zip: false }))).toBe("服务器上没有 zip 命令");
  });

  it("解压：tar 系要 tar，zip 要 unzip", () => {
    expect(extractToolReason("tar_xz", caps({ tar: false }))).toBe("服务器上没有 tar 命令");
    expect(extractToolReason("tar_xz", caps({ unzip: false }))).toBeNull();
    expect(extractToolReason("zip", caps({ unzip: false }))).toBe("服务器上没有 unzip 命令");
    expect(extractToolReason("zip", caps({ zip: false }))).toBeNull();
  });
});

describe("提示条和结束提示", () => {
  it("处理中的文案", () => {
    expect(archiveBusyText("compress", 3)).toBe("压缩中…（已 3 秒）");
    expect(archiveBusyText("compress", 3, 5 * 1024 * 1024)).toBe("压缩中…（已写入 5.0 MB · 已 3 秒）");
    expect(archiveBusyText("scan", 1)).toBe("检查压缩包中…（已 1 秒）");
    expect(archiveBusyText("extract", 12)).toBe("解压中…（已 12 秒）");
  });

  it("压缩结束", () => {
    expect(compressNotice({ state: "ok", name: "logs.tar.gz", path: "/srv/logs.tar.gz" })).toEqual({
      type: "success",
      text: "已压缩为「logs.tar.gz」",
    });
    expect(
      compressNotice({ state: "ok", name: "a.tar.gz", warning: "有文件在打包期间被改动过" }).text,
    ).toBe("已压缩为「a.tar.gz」（有文件在打包期间被改动过）");
    expect(compressNotice({ state: "cancelled" })).toEqual({
      type: "info",
      text: "已取消压缩，没有留下临时文件",
    });
  });

  it("解压结束：写清楚解出多少、跳过多少", () => {
    expect(extractNotice({ state: "ok", extracted: 10, skipped: 0 }, "site", false)).toEqual({
      type: "success",
      text: "已解压到「site」：解出 10 个文件",
    });
    expect(extractNotice({ state: "ok", extracted: 7, skipped: 3 }, "site", true).text).toBe(
      "已解压到「site」：解出 7 个文件，跳过 3 个已有文件",
    );
    expect(
      extractNotice({ state: "ok", extracted: 1, skipped: 0, warning: "有警告" }, "x", false).text,
    ).toContain("（有警告）");
  });

  it("取消解压：新建的文件夹不留半成品；解到已有文件夹时说清楚已解出的保留", () => {
    expect(extractNotice({ state: "cancelled", extracted: 0, skipped: 0 }, "site", false).text).toBe(
      "已取消解压，没有留下半成品",
    );
    expect(extractNotice({ state: "cancelled", extracted: 0, skipped: 0 }, "site", true).text).toBe(
      "已取消解压，已经解出的文件保留在「site」里",
    );
  });

  it("冲突名字列不全时写「等 N 个」", () => {
    const plan = (count: number, names: string[]): Pick<ExtractPlan, "conflict_count" | "conflict_sample"> => ({
      conflict_count: count,
      conflict_sample: names,
    });
    expect(conflictMoreText(plan(2, ["a", "b"]))).toBeNull();
    expect(conflictMoreText(plan(12, ["a", "b", "c", "d", "e"]))).toBe("等 12 个");
  });
});
