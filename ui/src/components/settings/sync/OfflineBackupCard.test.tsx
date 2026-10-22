/** @vitest-environment jsdom */
//! 离线备份卡片：导出要两次密码一致、至少 8 位；导入默认合并，可以选替换。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  syncOfflineExport: vi.fn(async () => "已导出"),
  syncOfflineImport: vi.fn(async () => ({
    summary: "已导入：主机新增 2",
    safety_backup: "zssh-backup-x.bin",
    status: {},
  })),
}));
vi.mock("../../../api/sync", () => api);
vi.mock("../../../api/dialog", () => ({
  pickBackupFile: vi.fn(async () => "D:\\backup\\test.zsshbak"),
  pickBackupSavePath: vi.fn(async () => "D:\\backup\\out.zsshbak"),
}));
vi.mock("../../../stores/sync", () => ({
  useSync: (select: (state: { apply: () => void }) => unknown) => select({ apply: () => {} }),
  refreshLocalData: vi.fn(async () => {}),
}));

import OfflineBackupCard from "./OfflineBackupCard";

afterEach(cleanup);
beforeEach(() => vi.clearAllMocks());

function typeInto(index: number, value: string) {
  const inputs = document.querySelectorAll<HTMLInputElement>('input[type="password"]');
  fireEvent.change(inputs[index], { target: { value } });
}

describe("离线备份", () => {
  it("导出：密码太短或两次不一致不导出，一致才导出", async () => {
    render(<OfflineBackupCard />);
    await act(async () => screen.getByRole("button", { name: "导出…" }).click());
    typeInto(0, "short");
    typeInto(1, "short");
    await act(async () => screen.getByRole("button", { name: "导出" }).click());
    expect(screen.getByRole("alert").textContent).toContain("至少 8");
    typeInto(0, "long-enough-1");
    typeInto(1, "long-enough-2");
    await act(async () => screen.getByRole("button", { name: "导出" }).click());
    expect(screen.getByRole("alert").textContent).toContain("不一样");
    expect(api.syncOfflineExport).not.toHaveBeenCalled();
    typeInto(1, "long-enough-1");
    await act(async () => screen.getByRole("button", { name: "导出" }).click());
    expect(api.syncOfflineExport).toHaveBeenCalledWith("D:\\backup\\out.zsshbak", "long-enough-1");
    expect(screen.getByTestId("offline-message").textContent).toBe("已导出");
  });

  it("导入：默认合并，选了替换就按替换", async () => {
    render(<OfflineBackupCard />);
    await act(async () => screen.getByRole("button", { name: "导入…" }).click());
    typeInto(0, "file-password");
    fireEvent.click(screen.getByText("替换"));
    await act(async () => screen.getByRole("button", { name: "替换导入" }).click());
    expect(api.syncOfflineImport).toHaveBeenCalledWith(
      "D:\\backup\\test.zsshbak",
      "file-password",
      "replace",
    );
    expect(screen.getByTestId("offline-message").textContent).toContain("主机新增 2");
  });
});
