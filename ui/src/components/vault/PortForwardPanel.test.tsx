/** @vitest-environment jsdom */
//! 端口转发面板：按主机过滤、显示状态、开 / 停切换、新建保存。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ForwardStatus, PortForward } from "../../api/forwards";

const RULES: PortForward[] = [
  {
    id: 1,
    host_id: 3,
    name: "数据库",
    kind: "local",
    bind_host: "127.0.0.1",
    bind_port: 15432,
    dest_host: "localhost",
    dest_port: 5432,
    auto_start: false,
  },
  {
    id: 2,
    host_id: 4,
    name: "别的主机",
    kind: "remote",
    bind_host: "127.0.0.1",
    bind_port: 9000,
    dest_host: "localhost",
    dest_port: 3000,
    auto_start: true,
  },
];

let statuses: ForwardStatus[] = [];

vi.mock("../../api/forwards", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/forwards")>();
  return {
    ...actual,
    forwardList: vi.fn(async () => RULES),
    forwardStatuses: vi.fn(async () => statuses),
    forwardStart: vi.fn(async () => {}),
    forwardStop: vi.fn(async () => {}),
    forwardSave: vi.fn(async (draft: unknown) => draft),
    forwardDelete: vi.fn(async () => {}),
    onForwardStatus: vi.fn(async () => () => {}),
  };
});
vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));

import PortForwardPanel from "./PortForwardPanel";
import { useVault } from "../../stores/vault";

afterEach(cleanup);

beforeEach(() => {
  vi.clearAllMocks();
  statuses = [];
  useVault.setState({ loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("端口转发面板", () => {
  it("只列出这台主机的规则，点开启调 forwardStart", async () => {
    const api = await import("../../api/forwards");
    render(<PortForwardPanel hostId={3} onClose={() => {}} onError={() => {}} />);
    await act(async () => {});
    expect(screen.getByText("数据库")).toBeTruthy();
    expect(screen.queryByText("别的主机")).toBeNull();
    expect(screen.getByText("未开启")).toBeTruthy();
    await act(async () => screen.getByTitle("开启").click());
    expect(api.forwardStart).toHaveBeenCalledWith(1);
  });

  it("运行中显示计数，点停止调 forwardStop；出错显示原因", async () => {
    const api = await import("../../api/forwards");
    statuses = [
      {
        id: 1,
        phase: "running",
        error: null,
        listening_port: null,
        counters: { active: 2, total: 5, bytes_sent: 2048, bytes_received: 10 },
      },
      {
        id: 2,
        phase: "failed",
        error: "本机端口 9000 已经被别的程序占用",
        listening_port: null,
        counters: { active: 0, total: 0, bytes_sent: 0, bytes_received: 0 },
      },
    ];
    render(<PortForwardPanel hostId={null} onClose={() => {}} onError={() => {}} />);
    await act(async () => {});
    expect(screen.getByText("运行中")).toBeTruthy();
    expect(screen.getByText(/当前 2 条连接/)).toBeTruthy();
    expect(screen.getByText("本机端口 9000 已经被别的程序占用")).toBeTruthy();
    await act(async () => screen.getByTitle("停止").click());
    expect(api.forwardStop).toHaveBeenCalledWith(1);
  });

  it("新建规则带上当前主机", async () => {
    const api = await import("../../api/forwards");
    render(<PortForwardPanel hostId={3} onClose={() => {}} onError={() => {}} />);
    await act(async () => {});
    await act(async () => screen.getByRole("button", { name: "新建" }).click());
    await act(async () => screen.getByRole("button", { name: "保存" }).click());
    expect(api.forwardSave).toHaveBeenCalledWith(
      expect.objectContaining({ id: null, host_id: 3, kind: "local" }),
    );
  });
});
