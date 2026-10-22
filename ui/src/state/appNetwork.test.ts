import { describe, expect, it } from "vitest";

import { appProxyChoice, appProxyValue, systemProxyText } from "./appNetwork";

describe("软件网络代理设置", () => {
  it("没设置、读不懂、指向已删除的代理都按使用 SSH 默认代理", () => {
    expect(appProxyChoice(null, [])).toBe("ssh_default");
    expect(appProxyChoice("not json", [])).toBe("ssh_default");
    expect(appProxyChoice(JSON.stringify("proxy:9"), [1, 2])).toBe("ssh_default");
  });

  it("四种取值来回不变", () => {
    for (const choice of ["ssh_default", "system", "none", "proxy:2"]) {
      expect(appProxyChoice(appProxyValue(choice), [2])).toBe(choice);
    }
  });

  it("系统代理的说明文字", () => {
    expect(systemProxyText({ kind: "manual", scheme: "http", host: "127.0.0.1", port: 7890 })).toBe(
      "系统代理：127.0.0.1:7890",
    );
    expect(systemProxyText({ kind: "manual", scheme: "socks5", host: "127.0.0.1", port: 1080 })).toBe(
      "系统代理：SOCKS5 127.0.0.1:1080",
    );
    expect(systemProxyText({ kind: "auto_config", url: "http://wpad/x.pac" })).toContain("PAC");
    expect(systemProxyText({ kind: "none" })).toContain("没有设置代理");
  });
});
