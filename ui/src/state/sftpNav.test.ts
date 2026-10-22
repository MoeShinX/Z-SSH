import { describe, expect, it } from "vitest";

import {
  canBack,
  canForward,
  current,
  EMPTY_HISTORY,
  forget,
  ListingCache,
  step,
  visit,
} from "./sftpNav";

describe("SFTP 浏览历史", () => {
  it("进目录、后退、前进，像浏览器一样", () => {
    let h = visit(visit(visit(EMPTY_HISTORY, "/"), "/root"), "/root/a");
    expect(canBack(h)).toBe(true);
    expect(canForward(h)).toBe(false);
    const back = step(h, -1);
    expect(back?.path).toBe("/root");
    h = back!.history;
    expect(canForward(h)).toBe(true);
    expect(step(h, 1)?.path).toBe("/root/a");
  });

  it("后退之后进了别的目录，前进那一截丢掉", () => {
    let h = visit(visit(visit(EMPTY_HISTORY, "/"), "/a"), "/b");
    h = step(h, -1)!.history;
    h = visit(h, "/c");
    expect(h.stack).toEqual(["/", "/a", "/c"]);
    expect(canForward(h)).toBe(false);
  });

  it("刷新同一个目录不算一步；走到头返回 null", () => {
    const h = visit(visit(EMPTY_HISTORY, "/"), "/");
    expect(h.stack).toEqual(["/"]);
    expect(step(h, -1)).toBeNull();
    expect(step(h, 1)).toBeNull();
  });

  it("去不了的目录从历史里拿掉", () => {
    const h = forget(visit(visit(visit(EMPTY_HISTORY, "/"), "/gone"), "/b"), "/gone");
    expect(h.stack).toEqual(["/", "/b"]);
    expect(current(h)).toBe("/b");
  });
});

describe("目录列表缓存", () => {
  it("超过上限丢最久没用的，读一次算用过", () => {
    const cache = new ListingCache<number>(2);
    cache.set("/a", 1);
    cache.set("/b", 2);
    cache.get("/a");
    cache.set("/c", 3);
    expect(cache.get("/b")).toBeUndefined();
    expect(cache.get("/a")).toBe(1);
    expect(cache.get("/c")).toBe(3);
  });
});
