/** @vitest-environment jsdom */
//! 空白标签选主机：搜索过滤、点一行连、SFTP 按钮另开、回车连第一项、方向键挪光标。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));

import type { Host } from "../../api/vault";
import { useVault } from "../../stores/vault";
import EmptyTabLauncher, { filterHosts } from "./EmptyTabLauncher";

const host = (id: number, name: string, tags: string[] = []) =>
  ({ id, name, host: `10.0.0.${id}`, username: "root", port: 22, tags }) as unknown as Host;

afterEach(cleanup);
beforeEach(() => {
  useVault.setState({ hosts: [host(2, "web-1", ["生产"]), host(1, "db-1")] });
});

describe("空白标签选主机", () => {
  it("按名字排序，按名字 / 地址 / 标签过滤", () => {
    const hosts = useVault.getState().hosts;
    expect(filterHosts(hosts, "").map((h) => h.name)).toEqual(["db-1", "web-1"]);
    expect(filterHosts(hosts, "生产").map((h) => h.name)).toEqual(["web-1"]);
    expect(filterHosts(hosts, "10.0.0.1").map((h) => h.name)).toEqual(["db-1"]);
  });

  it("点一行连接，点 SFTP 另开；回车连光标所在那项", () => {
    const onConnect = vi.fn();
    const onOpenSftp = vi.fn();
    render(<EmptyTabLauncher onConnect={onConnect} onOpenSftp={onOpenSftp} />);
    fireEvent.click(screen.getByText("web-1"));
    expect(onConnect).toHaveBeenLastCalledWith(expect.objectContaining({ name: "web-1" }));
    const sftp = screen.getAllByTitle("打开 SFTP（新标签）")[0];
    fireEvent.click(sftp);
    expect(onOpenSftp).toHaveBeenCalledWith(expect.objectContaining({ name: "db-1" }));
    expect(onConnect).toHaveBeenCalledTimes(1);

    const input = screen.getByPlaceholderText(/搜索主机名/);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onConnect).toHaveBeenLastCalledWith(expect.objectContaining({ name: "web-1" }));
  });

  it("没有主机时提示去主机页新建", () => {
    useVault.setState({ hosts: [] });
    render(<EmptyTabLauncher onConnect={() => {}} onOpenSftp={() => {}} />);
    expect(screen.getByText(/主机库里还没有主机/)).toBeTruthy();
  });
});
