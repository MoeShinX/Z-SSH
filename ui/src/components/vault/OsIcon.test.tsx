/** @vitest-environment jsdom */
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { useHostOs } from "../../stores/hostOs";
import OsIcon from "./OsIcon";

afterEach(() => {
  cleanup();
  useHostOs.setState({ byHost: {} });
});

describe("OsIcon", () => {
  it("探测到系统就显示对应的标志，没探测到用 Z-SSH 图标", () => {
    useHostOs.setState({ byHost: { 7: "debian" } });
    const known = render(<OsIcon hostId={7} size="tree" />);
    const badge = known.container.querySelector("[data-os]");
    expect(badge?.getAttribute("data-os")).toBe("debian");
    expect(badge?.getAttribute("title")).toBe("Debian");
    expect(badge?.querySelector("img")?.getAttribute("src")).toContain("Debian");

    const unknown = render(<OsIcon hostId={8} size="card" />);
    expect(unknown.container.querySelector("[data-os]")).toBeNull();
    expect(unknown.container.querySelector("img")?.getAttribute("src")).toContain("zssh-z-neon/icon.ico");
  });

  it("后端给了图标里没有的系统 ID：退回 Z-SSH 图标", () => {
    useHostOs.setState({ byHost: { 7: "solaris" } });
    const view = render(<OsIcon hostId={7} size="tree" />);
    expect(view.container.querySelector("[data-os]")).toBeNull();
    expect(view.container.querySelector("img")?.getAttribute("src")).toContain("zssh-z-neon/icon.ico");
  });
});
