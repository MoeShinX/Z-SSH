// 状态栏采集租约（M1-10）的生命周期规则（C13）。
//
// 为什么单独抽出来：原来关掉"服务器状态"开关时 effect 直接 return，已有租约不退，
// 后台采集照跑；订阅请求的回调又捕获着当时的 true，开关关了之后回来照样登记新租约。
// 这里把"哪些租约该退""异步回来的订阅还要不要"写成纯函数，App 只负责执行。

/** 一个 pane 持有的采集订阅。记下订阅时的会话：pane 重连换了会话，旧订阅就作废。 */
export interface StatsLease {
  subscriptionId: number;
  sessionId: string;
}

/**
 * 该退掉的租约（返回 pane 列表）。
 *
 * 开关关着：全部退。开着：只保留"焦点 pane 且还是订阅时那个会话"的那一条。
 */
export function leasesToRelease<P>(
  leases: ReadonlyMap<P, StatsLease>,
  enabled: boolean,
  focusPane: P | null,
  focusSession: string | null,
): P[] {
  const release: P[] = [];
  for (const [pane, lease] of leases) {
    const keep =
      enabled && focusPane !== null && pane === focusPane && lease.sessionId === focusSession;
    if (!keep) release.push(pane);
  }
  return release;
}

/** 发出订阅请求那一刻的情况。 */
export interface StatsRequest<P> {
  generation: number;
  pane: P;
  sessionId: string;
}

/** 订阅结果回来时的当前情况。 */
export interface StatsNow {
  /** 当前代次：每次关掉开关（或界面卸载）加一，之前发出的请求全部作废。 */
  generation: number;
  /** 这个 pane 现在背后的会话（pane 已关闭为 null）。 */
  paneSession: string | null;
  /** 这个 pane 是不是已经有别的请求登记了租约（重复请求时只留先登记的）。 */
  alreadyLeased: boolean;
}

/** 异步回来的订阅还要不要；不要的调用方应立刻退订。 */
export function keepSubscription<P>(request: StatsRequest<P>, now: StatsNow): boolean {
  return (
    request.generation === now.generation &&
    now.paneSession === request.sessionId &&
    !now.alreadyLeased
  );
}
