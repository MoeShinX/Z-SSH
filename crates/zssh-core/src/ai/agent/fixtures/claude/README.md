# Claude Code 驱动参考（AI-4）

- `claude_turn.jsonl`：真实回合录下来的 stream-json 消息（开发时实测的版本：
  Claude Code 2.1.300，2026-10-16）。清洗过：用户名/路径换成占位（`C:\data\...`）、
  会话 ID 和每条消息的 `uuid` / `api_message_id` / `request_id` 都换成固定假值、
  命名管道名换成假值、去掉与协议无关的 `rate_limit_event`。`claude::translate`
  的单测拿它当输入。
- 最低支持版本 **2.0.41**：`--tools ""`（卡里要求用来关掉全部内置工具）是 2.0.41 才有
  的（2.0.40 及以前没有这个参数）；2.0.41 上把完整启动配方 + 一次真实会话（MCP 工具
  调用、本机命令被拒、中断控制消息）实测跑通，依据见报告 AI-4 第 1 节。
- 版本差异：`--permission-prompts none`（2.1.x 才有）和 `--effort`（2.1.x 才有）按
  `--help` 探测结果**按需追加**（未知参数会让 claude 直接报错退出，实测）。
