import { describe, expect, it } from "vitest";

import type { ConflictView } from "../api/sync";
import { makeStatus, PENDING } from "./syncFixtures";
import {
  concurrencyNotice,
  deletionLines,
  deviceLabel,
  entityLabel,
  keptText,
  lastResultText,
  otherText,
  passwordStrength,
  statusBarModel,
} from "./syncModel";

describe("statusBarModel", () => {
  it("没连接存储时不显示", () => {
    expect(statusBarModel(null).visible).toBe(false);
    expect(statusBarModel(makeStatus({ configured: false, config: null })).visible).toBe(false);
  });

  it("每种状态各有一条文案和色调", () => {
    expect(statusBarModel(makeStatus({ running: true }))).toMatchObject({
      tone: "busy",
      text: "同步中…",
    });
    expect(statusBarModel(makeStatus()).tone).toBe("ok");
    expect(statusBarModel(makeStatus()).text).toMatch(/^已同步 \d\d:\d\d$/);
    expect(statusBarModel(makeStatus({ ready: false })).text).toContain("还没有设置主密码");
    expect(statusBarModel(makeStatus({ password_in_memory: false })).text).toContain(
      "需要输入主密码",
    );
    expect(statusBarModel(makeStatus({ last: null })).text).toBe("同步：尚未同步");
    expect(statusBarModel(makeStatus({ baseline_broken: true })).tone).toBe("error");
  });

  it("失败原因直接写在状态栏里", () => {
    const status = makeStatus({
      last: { at: 1, status: "error", summary: "无法连接到存储服务", version: null, conflicts: 0, auto: true },
    });
    expect(statusBarModel(status)).toEqual({
      visible: true,
      tone: "error",
      text: "同步失败：无法连接到存储服务",
    });
  });

  it("凭据被拒后自动同步暂停，状态栏按存储种类说明要重新填什么", () => {
    const base = makeStatus().config!;
    const github = makeStatus({ auth_paused: "401", config: { ...base, provider: "github" } });
    expect(statusBarModel(github).text).toBe("同步已暂停：令牌已失效，请重新填写");
    expect(statusBarModel(github).tone).toBe("error");
    const webdav = makeStatus({ auth_paused: "401" });
    expect(statusBarModel(webdav).text).toContain("同步已暂停：账号或密码已失效");
    const s3 = makeStatus({ auth_paused: "x", config: { ...base, provider: "s3" } });
    expect(statusBarModel(s3).text).toContain("访问密钥");
  });

  it("OneDrive 登录失效时状态栏说“请重新登录”，不是“重新填写”", () => {
    const base = makeStatus().config!;
    const onedrive = makeStatus({
      auth_paused: "OneDrive 登录已失效，请重新登录",
      config: { ...base, provider: "onedrive", account: "Test User" },
    });
    expect(statusBarModel(onedrive)).toEqual({
      visible: true,
      tone: "error",
      text: "同步已暂停：OneDrive 登录已失效，请重新登录",
    });
  });

  it("自动同步停在防误删确认上要明说，并且优先于其他状态", () => {
    const status = makeStatus({ pending: { ...PENDING, auto: true } });
    const model = statusBarModel(status);
    expect(model.tone).toBe("warn");
    expect(model.text).toContain("自动同步已暂停");
    expect(model.text).toContain("12");
  });

  it("有待处理的冲突时提示条数", () => {
    const model = statusBarModel(makeStatus({ conflict_count: 2 }));
    expect(model.tone).toBe("warn");
    expect(model.text).toContain("2 条冲突待处理");
  });
});

describe("lastResultText / concurrencyNotice", () => {
  it("成功、失败、同步期间有改动", () => {
    expect(lastResultText(makeStatus())).toContain("同步成功，上传版本");
    expect(lastResultText(makeStatus({ last: null }))).toBe("还没有同步过");
    const failed = makeStatus({
      last: { at: 1_700_000_000, status: "error", summary: "主密码错误", version: null, conflicts: 0, auto: true },
    });
    expect(lastResultText(failed)).toContain("（自动） 失败：主密码错误");
    const changed = makeStatus({
      last: { at: 1_700_000_000, status: "local_changed", summary: "", version: null, conflicts: 0, auto: false },
    });
    expect(lastResultText(changed)).toContain("稍后自动重新同步");
  });

  it("不支持或没检测过并发保护都要明说，不能当成支持", () => {
    expect(concurrencyNotice(makeStatus())?.tone).toBe("ok");
    const unsupported = concurrencyNotice(
      makeStatus({ concurrency: { protected: false, notes: [], checked_at: 1 } }),
    );
    expect(unsupported?.tone).toBe("warn");
    expect(unsupported?.text).toContain("不支持并发保护");
    expect(unsupported?.text).toContain("很小的概率覆盖");
    const unknown = concurrencyNotice(makeStatus({ concurrency: null }));
    expect(unknown?.tone).toBe("warn");
    expect(unknown?.text).toContain("还没有检测");
    expect(concurrencyNotice(makeStatus({ configured: false }))).toBeNull();
  });
});

describe("passwordStrength", () => {
  it("按长度和字符种类分级", () => {
    expect(passwordStrength("").score).toBe(0);
    expect(passwordStrength("abc").label).toBe("太弱");
    expect(passwordStrength("abcdefgh").label).toBe("弱");
    expect(passwordStrength("abcdefgh1234").label).toBe("一般");
    expect(passwordStrength("Abcdef12!").label).toBe("一般");
    expect(passwordStrength("Correct-Horse-Battery-9").label).toBe("强");
    expect(passwordStrength("中文密码也算长度的").score).toBeGreaterThanOrEqual(2);
  });
});

describe("deletionLines / 冲突文案", () => {
  it("写清数量和前几条名字，其余折成“还有 N 条”", () => {
    const lines = deletionLines(PENDING);
    expect(lines.headline).toContain("删除 12 条记录");
    expect(lines.headline).toContain("主机 11 台");
    expect(lines.names).toEqual(["主机：web-1", "分组：生产"]);
    expect(lines.more).toBe(10);
  });

  const conflict: ConflictView = {
    id: 1,
    at: 1,
    entity: "host",
    key: "k",
    name: "web-1",
    kept_summary: "root@a:22",
    kept_device: "device-abcdef123456",
    kept_is_local: false,
    other_summary: "root@b:22",
    other_device: "dev-local",
    other_is_local: true,
    deletion: false,
    can_use_other: true,
    message: "",
  };

  it("说明哪台电脑的改动被保留、另一份是什么", () => {
    expect(keptText(conflict)).toBe("另一台电脑（device-a） 的改动：root@a:22");
    expect(otherText(conflict)).toBe("本机 的改动：root@b:22");
    const deletion = { ...conflict, deletion: true, kept_summary: "已删除" };
    expect(keptText(deletion)).toContain("删除");
    expect(otherText(deletion)).toContain("最后的内容");
  });

  it("类型名和设备名", () => {
    expect(entityLabel("known_host")).toBe("主机指纹");
    expect(entityLabel("identity")).toBe("凭据");
    expect(deviceLabel(true, null)).toBe("本机");
    expect(deviceLabel(false, null)).toBe("另一台电脑");
  });
});
