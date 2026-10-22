/** @vitest-environment jsdom */
//! 关于页的更新下载进度条：知道总大小时是确定进度 + 百分比和大小，不知道时是不确定样式。

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { UpdateStatus } from "../../api/updater";
import UpdateProgress from "./UpdateProgress";

const MB = 1024 * 1024;

afterEach(() => cleanup());

describe("UpdateProgress", () => {
  it("知道总大小：进度条宽度和文字都是百分比", () => {
    const status: UpdateStatus = {
      phase: "downloading",
      version: "0.1.3",
      downloaded: 5 * MB,
      total: 10 * MB,
    };
    render(<UpdateProgress status={status} />);
    const bar = screen.getByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("50");
    const fill = bar.firstElementChild as HTMLElement;
    expect(fill.style.width).toBe("50%");
    expect(fill.dataset.indeterminate).toBeUndefined();
    expect(screen.getByText("正在下载 v0.1.3：50%（5.0 MB / 10.0 MB）")).toBeTruthy();
  });

  it("不知道总大小：不确定样式，只写已下载的字节数", () => {
    render(
      <UpdateProgress
        status={{ phase: "downloading", version: "0.1.3", downloaded: 3 * MB, total: null }}
      />,
    );
    const bar = screen.getByRole("progressbar");
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    expect((bar.firstElementChild as HTMLElement).dataset.indeterminate).toBe("true");
    expect(screen.getByText("正在下载 v0.1.3：已下载 3.0 MB")).toBeTruthy();
  });

  it("不在下载时什么都不画", () => {
    const { container } = render(<UpdateProgress status={{ phase: "ready", version: "0.1.3" }} />);
    expect(container.innerHTML).toBe("");
  });
});
