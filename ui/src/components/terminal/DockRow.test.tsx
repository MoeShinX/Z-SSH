/** @vitest-environment jsdom */
//! 终端｜AI｜SFTP 的顺序固定（M3-3 第 6.1 节）：不管开关组合，位置都不变。

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DOCK_ORDER } from "../../state/dockLayout";
import DockRow from "./DockRow";

function order(container: HTMLElement): string[] {
  const row = container.querySelector('[data-section="dock-row"]');
  return Array.from(row?.children ?? []).map((child) => child.getAttribute("data-slot") ?? "");
}

const main = <div data-slot="terminal" />;
const ai = <div data-slot="ai" />;
const sftp = <div data-slot="sftp" />;

describe("DockRow", () => {
  it("三块都开：终端、AI、SFTP", () => {
    const { container } = render(<DockRow main={main} ai={ai} sftp={sftp} />);
    expect(order(container)).toEqual([...DOCK_ORDER]);
  });

  it("只开 SFTP：紧挨终端；只开 AI：AI 在终端右边", () => {
    expect(order(render(<DockRow main={main} ai={null} sftp={sftp} />).container)).toEqual([
      "terminal",
      "sftp",
    ]);
    expect(order(render(<DockRow main={main} ai={ai} sftp={null} />).container)).toEqual([
      "terminal",
      "ai",
    ]);
  });

  it("传参顺序写反也不会换位置", () => {
    const { container } = render(<DockRow sftp={sftp} ai={ai} main={main} />);
    expect(order(container)).toEqual(["terminal", "ai", "sftp"]);
  });
});
