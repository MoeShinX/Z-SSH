import { describe, expect, it } from "vitest";

import { hostCopyText, normalizeHostInput } from "./hostAddress";

describe("normalizeHostInput", () => {
  it("去掉 IPv6 的方括号和首尾空白", () => {
    expect(normalizeHostInput(" [2001:db8::1] ")).toBe("2001:db8::1");
    expect(normalizeHostInput("[fe80::1%3]")).toBe("fe80::1%3");
  });

  it("裸 IPv6、IPv4、域名原样保留", () => {
    expect(normalizeHostInput("2001:db8::1")).toBe("2001:db8::1");
    expect(normalizeHostInput("10.0.0.1")).toBe("10.0.0.1");
    expect(normalizeHostInput("example.com")).toBe("example.com");
  });

  it("方括号里不是 IPv6 的不动", () => {
    expect(normalizeHostInput("[example.com]")).toBe("[example.com]");
  });
});

describe("hostCopyText", () => {
  it("复制主机名就是列表里显示的名字", () => {
    expect(hostCopyText({ name: "web-sg", host: "10.0.0.1" }, "name")).toBe("web-sg");
  });

  it("复制地址只有地址本身，不带用户名和端口", () => {
    expect(hostCopyText({ name: "web-sg", host: "10.0.0.1" }, "address")).toBe("10.0.0.1");
    expect(hostCopyText({ name: "web-sg", host: "example.com" }, "address")).toBe("example.com");
    expect(hostCopyText({ name: "web-sg", host: "2001:db8::1" }, "address")).toBe("2001:db8::1");
  });

  it("老数据里带方括号的 IPv6 复制出来不带方括号", () => {
    expect(hostCopyText({ name: "v6", host: "[::1]" }, "address")).toBe("::1");
  });
});
