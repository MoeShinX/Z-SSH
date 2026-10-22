# codex 协议参考（AI-3）

这里的文件是 **codex 0.162.1** 用官方命令导出的 app-server 协议，供实现和单测参考。

```bash
# 生成命令（--out 指向本目录；--experimental 会把实验性方法也导出来，我们不用）
codex app-server generate-json-schema --out crates/zssh-core/src/ai/agent/fixtures/codex
```

- `codex_app_server_protocol.schemas.json`：主 bundle（`InitializeParams`、审批请求 /
  应答、`ClientRequest` / `ServerRequest` / `ServerNotification` 的联合类型）。
- `codex_app_server_protocol.v2.schemas.json`：v2 bundle（`ThreadStartParams`、
  `TurnStartParams`、`ModelListParams`、`AccountReadParams` 等）。
- `codex_turn_events.json`：真实回合录下来的通知样本（在测试环境的独立数据目录里跑
  了一个只读回合，清洗掉本机路径后入库），`codex::translate` 的单测拿它当输入。

单测（`codex::protocol` / `codex::translate`）会检查：

- 我们发送 / 应答的**方法名**都出现在导出的 schema 里；
- 我们构造的**参数键**都是 schema 里存在的属性（拼错或上游改名会被拦住）；
- 事件翻译能吃完录制的样本，不 panic、不产生空事件。

驱动支持的最低 codex 版本是 **0.144.0**（依据：
0.144.0 的导出 schema 覆盖我们用到的全部方法与字段，且 `--disable` / `features list` /
`mcp list --json` / `app-server --stdio` 都在该版本上实测可用）。
