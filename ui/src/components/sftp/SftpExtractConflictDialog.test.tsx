/** @vitest-environment jsdom */
//! 解压遇到同名文件的对话框（UI-13）：显示冲突个数和前几个名字，三个按钮各自给出对应的选择。

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import SftpExtractConflictDialog from "./SftpExtractConflictDialog";

afterEach(() => cleanup());

function renderDialog(plan = { conflict_count: 2, conflict_sample: ["site/a.txt", "site/b.txt"] }) {
  const onChoose = vi.fn();
  const onCancel = vi.fn();
  render(
    <SftpExtractConflictDialog
      archiveName="site.tar.gz"
      destName="site"
      plan={plan}
      onChoose={onChoose}
      onCancel={onCancel}
    />,
  );
  return { onChoose, onCancel };
}

const button = (action: string) =>
  document.querySelector(`[data-action="${action}"]`) as HTMLButtonElement;

describe("解压同名文件对话框", () => {
  it("写明压缩包、目标文件夹、冲突个数和名字", () => {
    renderDialog();
    const text = document.body.textContent ?? "";
    expect(text).toContain("目标里已有同名文件");
    expect(text).toContain("解压「site.tar.gz」到「site」时，有 2 个文件在里面已经存在");
    const names = [...document.querySelectorAll('[data-section="extract-conflict-names"] li')].map(
      (li) => li.textContent,
    );
    expect(names).toEqual(["site/a.txt", "site/b.txt"]);
    expect(document.querySelector('[data-section="extract-conflict-more"]')).toBeNull();
  });

  it("名字没列全时最后一行写「…等 N 个」", () => {
    renderDialog({
      conflict_count: 12,
      conflict_sample: ["a", "b", "c", "d", "e"],
    });
    expect(document.querySelectorAll('[data-section="extract-conflict-names"] li').length).toBe(6);
    expect(document.querySelector('[data-section="extract-conflict-more"]')?.textContent).toBe(
      "…等 12 个",
    );
  });

  it("三个按钮：覆盖、跳过已有文件、取消", () => {
    const { onChoose, onCancel } = renderDialog();
    expect(button("extract-overwrite").textContent).toBe("覆盖");
    expect(button("extract-skip").textContent).toBe("跳过已有文件");
    expect(button("extract-cancel").textContent).toBe("取消");

    fireEvent.click(button("extract-overwrite"));
    expect(onChoose).toHaveBeenLastCalledWith("overwrite");
    fireEvent.click(button("extract-skip"));
    expect(onChoose).toHaveBeenLastCalledWith("skip_existing");
    expect(onChoose).toHaveBeenCalledTimes(2);
    expect(onCancel).not.toHaveBeenCalled();

    fireEvent.click(button("extract-cancel"));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onChoose).toHaveBeenCalledTimes(2);
  });

  it("按 Esc 等于取消，什么都不解压", () => {
    const { onChoose, onCancel } = renderDialog();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onChoose).not.toHaveBeenCalled();
  });

  it("打开时焦点在不会丢数据的「跳过已有文件」上", () => {
    renderDialog();
    expect(document.activeElement).toBe(button("extract-skip"));
  });
});
