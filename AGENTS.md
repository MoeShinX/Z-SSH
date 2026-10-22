# Z-SSH 开发约定

Z-SSH 是一款只面向 Windows 10/11 x64 的 SSH 客户端：终端、SFTP、远端备份、AI 运维助手。技术栈是 Rust + Tauri 2 + React + xterm.js。

## 分工

- 设计和验收由另一方负责。设计文档在 `docs/`，每次的任务以任务卡的形式交给你（任务卡不在公开仓库里）。
- 你负责按任务卡实现。**一次只做一张任务卡**，做完写报告，然后停下等验收。验收通过后才会给下一张。
- 现阶段先把整体功能做出来。**需要人工点界面的手动验证，统一放到整个 M1 最后**，各张卡里不用做。能自动化的检查（单元测试、集成测试、example）照常做。
- 某张卡的部分内容做不了（比如依赖的库不支持），在报告里写清原因和替代方案，把其余部分做完。
- 任务卡里没写的功能不要加。觉得设计有问题，写进报告的"疑问与偏差"，不要自行改设计。
- 实现必须服从 `docs/M1-design.md`。任务卡和设计文档冲突时，以任务卡为准，并在报告里指出冲突。

## 参考代码

- **Netcatty** 是同类开源项目（Electron + React + xterm.js），在本地另有一份检出（放在工作区外面，路径按自己的环境来，**不要提交进本仓库**）。它的前端代码（终端、SFTP 界面、主题等）可以直接借鉴或移植过来；它的 SSH、SFTP 等后端逻辑可以作为 Rust 实现的参考。
- 后端必须是 Rust，不能引入 Electron、Node.js 运行时或 node-pty 这类 Node 原生模块。
- 移植过来的文件，文件头加一行注释写明来源，例如 `// Adapted from Netcatty components/Terminal.tsx`。
- **许可（用户决定：方案 A）**：Z-SSH 以 GPL-3.0-or-later 开源，正常署名 Netcatty。仓库根目录的 `LICENSE` 是 GPL-3.0 全文，`NOTICE` 列出第三方代码。规则：
  - **已有的来源注释一律不准删**
  - 移植了 Netcatty 新模块时，在 `NOTICE` 的 Netcatty 一节里补上
  - 界面上不出现 Netcatty 字样，致谢只放在 `NOTICE`、README 和关于页的"开源许可"里
- **内置编辑器依赖（M3-5）**：CodeMirror 6 的 34 个 npm 包（`@codemirror/*`、`@lezer/*`、`@marijn/find-cluster-break`、`crelt`、`style-mod`、`w3c-keyname`）全部是 MIT，已记在 `NOTICE`。再加新的编辑器相关依赖前先查它的许可，非 MIT / BSD / Apache-2.0 类的不要加。
- 只移植当前任务卡需要的部分，不要整块搬它的功能。设计文档里砍掉的功能（Mosh、串口、插件、云同步等）不要带进来。

## 环境

- 开发环境：Windows 10/11 x64、Rust stable（MSVC 工具链）、tauri-cli 2.x、Node + npm、VS 2022 C++ 生成工具、WebView2 运行时。
- **构建不能依赖 CMake / NASM**（不是每台机器都有）：`russh` 必须关掉默认特性，用 `ring` 后端。
- cargo、npm 下载失败时按自己的环境走本地代理（例如 `HTTPS_PROXY=http://127.0.0.1:<你的端口>`）。

## 可移植性

Z-SSH 以后要公开，代码要能在**别人的 Windows 10/11** 上用，不能只照着开发这台电脑写。

- 要照顾的差异：**不同的用户名**（含空格、含中文，如 `C:\Users\张 三`）、不同的安装方式、不同版本、没装某个智能体、配置文件不存在或写法不同。
- 任务卡里写的"本机是 xx 版本 / 装在某处 / 配置里有 xx"只是**一个测试用例**，不是唯一要支持的情况：**实现按"一般情况"写**（版本按 `--help` / `--version` 探测，路径按候选清单找，配置按容错读），本机那种情况拿来实测。
- 代码、注释、测试、样本文件、文档、报告里**不写开发机特有的信息**：用户名、用户目录路径、用户自己配的 MCP 服务名或其他配置项名、本机装了哪些软件（版本号可以写，那是"开发时实测的版本"）。举例用中性的占位：`C:\Users\<用户>`、`my_mcp_server`、`web-1`。
- 依赖本机环境的测试（真实智能体、测试服务器、某个已登录的账号）在环境不满足时**跳过并打印原因**，不能失败；加了门槛的用例在门槛不满足时同样打印"跳过…"。测试里**不能断言只有这台电脑才成立的值**（比如"模型正好 40 个""默认模型叫 xx""路径一定在 `%APPDATA%`"）。
- 处理用户文件（配置、凭据、目录）时按"**读不了 = 不确定**"处理：不崩、不猜、不漏（该屏蔽的东西不能因为读不到就说没有），并且**只读不改**。
- 自动化脚本、测试、截图一律用独立数据目录，**不碰用户的真实数据**（见下面的截图隐私与数据目录规则）。

## 测试服务器

- 连接测试只从环境变量读取：`ZSSH_TEST_HOST`、`ZSSH_TEST_PORT`、`ZSSH_TEST_USER`、`ZSSH_TEST_PASSWORD`。
- 可选变量：`ZSSH_TEST_SOCKS5`（例如 `127.0.0.1:10808`，测代理）、`ZSSH_TEST_KEY_PATH` / `ZSSH_TEST_KEY_PASSPHRASE`（测私钥认证）。没设置就跳过对应测试。
- **任何密码、私钥、IP、主机名都不能写进代码、测试、提交、文档和报告。**报告里写"测试服务器"就行。
- 没设置这些环境变量时，依赖服务器的测试要跳过，并打印原因，不能算失败。
- **截图隐私（M1-10 出过事故；R-1 的关于页截图又漏过一次）**：截图前必须清理画面——新开会话后先 `PS1='$ '` 再 `clear`（`Last login ... from <IP>` 这行会暴露用户本机公网 IP）；画面里不能出现测试服务器的地址、主机名（`hostname` 的输出）、任何公网 IP、能看出服务器用途的进程名，以及**本机用户名和 `C:\Users\...` 这类用户目录路径**（设置页"关于"会显示数据目录，最容易被拍进去）。截图脚本要有**真正生效**的检查：读取 xterm 缓冲区文本和页面 DOM 文本，命中就让脚本失败，不能只靠人工看一眼；检查项里必须包含 `C:\Users\` 和当前用户名。截图脚本的 `ZSSH_DATA_DIR` 一律用不含用户名的目录（例如 `C:\zssh-shots\<场景>`），`datadir.mjs` 对带用户名的路径直接报错退出
- **报告必须真实**：报告里写"已检查""已通过"的每一项都必须实际做过、结论属实。写了没做或者结论不符，比没做更严重
- **自动化脚本和测试必须用独立的数据目录**：通过 `ZSSH_DATA_DIR` 覆盖（后端支持，默认仍是 `%APPDATA%\Z-ssh`）。脚本、截图、CDP 驱动一律不能碰用户的真实数据目录——用户可能在里面存了自己的主机。没设置就报错退出，不要回退到真实目录。
- **测试拉起 Claude Code 时不碰真实的 `~/.claude`**：测旧版本的 claude、联网测试、截图脚本让 Z-SSH 拉起 claude，一律给它单独设 `CLAUDE_CONFIG_DIR`（指向 `target/` 或 `C:\zssh-shots\<场景>` 下的临时目录），需要登录时只按字节复制 `.credentials.json` 进去，用完删掉。会拉起 claude 的测试必须要显式开启（`ZSSH_AGENT_E2E=1`），默认的 `cargo test` 和 CI 不能拉起它。产品本身运行时照常用用户自己的配置。

## 代码约定

- 业务逻辑放在 `crates/zssh-core`，它不能依赖 Tauri。`src-tauri` 只做命令和事件的转接。
- 错误用 `thiserror` 定义，不要在库代码里 `unwrap()` 或 `expect()`。测试和 example 里可以用。
- 日志用 `tracing`，不能打印密码、口令、私钥和 keyboard-interactive 的回答。
- 前端用 TypeScript strict 模式，不要用 `any`。
- 注释写为什么，不写做了什么。中文、英文都行，同一个文件里保持一致。

## 每次提交前必须全部通过

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ui && npm run build
cd ui && npm test
```

`npm run build` 必须包含 `tsc -b`，不能只跑 `vite build`。

**什么时候跑全套**（用户 2026-10-20 定的做法，省本机编译时间和磁盘）：

- 平时改完代码，本机只跑**改动相关**的测试（对应的 `cargo test -p … <过滤>`、`npx vitest run <文件>`），
  再加 `cargo fmt --all -- --check`、`cargo clippy`（改了 Rust 时）和 `npx tsc -b`（改了前端时）。
  **前端类型检查一定用 `tsc -b`**：`tsc --noEmit -p .` 不检查测试文件，2026-10-20 漏过一次，CI 才报出来。
- 推送后 GitHub 的 CI 会把五条门禁完整跑一遍。发版时推完版本号提交就可以打标签，让 CI 和 Release 同时跑
  （省 15～35 分钟）；**把草稿转成正式版之前，必须确认这次版本号提交的 CI 和 Release 都是绿的**。
  CI 没过：草稿不发布，删掉草稿和标签，修好后重打。
- 动了共享运行时、持久化、同步、发布流程，或者一次改动很大时，本机照样跑全套 `bash scripts/gates.sh`。
- 截图只在改了界面外观时才做（截图要单独编一个调试版程序）。
- `target/` 会越积越大，定期 `cargo clean`（只删编译产物）。

**五条门禁总耗时必须在 120 秒以内**（用户 2026-10-17 的要求；每张卡的报告写每条的退出码和总耗时）。
为此联网测试分两档：

- **门禁里只留冒烟组**：连接、开 shell、开 SFTP 并列一次目录、传一个小文件
  （`live_shell`、`sftp_live::sftp_channel_opens_on_the_existing_connection`、
  `sftp_transfer_live::upload_and_download_a_file_with_sha256`）。
- **其余联网测试**（SFTP 全套、断线续传、AI 驱动、认证、代理/跳板、连接池、流控死锁等）
  在 `common::full_live()` 里检查环境变量 `ZSSH_LIVE_FULL=1`，门禁里不设它，所以会打印
  "跳过…"直接返回；用 `powershell -File scripts/live-tests.ps1` 跑（它设置该变量并串行执行）。
  **每张卡的报告都要贴这个脚本的结果**（通过数、失败数、总耗时、退出码）。
- 新写的联网测试默认归"完整"档：开头加 `if !common::full_live("测试名") { return; }`；
  只有确实属于冒烟（≤ 几秒、不依赖大文件）的才留在门禁里。

`cargo test --workspace` **不含**依赖网速的吞吐/延迟门限测试：它们标了 `#[ignore]`
（刷屏隔离 `flood_isolation`、stats 往返延迟等）。这些测试要用
`powershell -File scripts/perf-tests.ps1`（即 `cargo test -- --ignored`）单独跑，
结果抄进报告。流控、缓冲、连接池相关的卡片**必须**跑一遍这个脚本。`flow_deadlock`
（验"会不会卡死"）现在也归完整档，在 `scripts/live-tests.ps1` 里跑。

链路慢时这些门限测试会失败——先确认网络再判代码问题（2026-10-14 掉到 1.3MB/s
时误报过一次）。

## Git

- 小步提交，一个提交做一件事。提交信息用 Conventional Commits，例如 `feat(core): open shell channel with pty`。
- **测试产物和临时文件不能提交**：截图输出、候选图标、补丁脚本、日志等。提交前看一眼 `git show --stat`。
- 只 `git add` 自己改过的具体文件，不要用 `git add -A` 或 `git add .`。
- 不要 push，不要改写已有提交的历史。

## 报告

每张任务卡完成后写一份报告（按任务编号命名，放在 `docs/reports/`；这个目录不进公开仓库），包含：

1. 完成了什么：对照任务卡逐条列出
2. 验收命令和输出：粘贴实际运行结果的关键部分，不要只写"已通过"。**五条门禁每条都要贴退出码**（PowerShell 用 `$LASTEXITCODE`）。vitest 有未处理错误时，即使测试全过退出码也是 1，算失败（AI-2c 出过事）
3. 每条写成"已完成"的功能，都要写明由哪条调用链触发（例如"面板停止按钮 → `ai_agent_abort` → …"）。只在单元测试里调用过、没接进真实入口的，不算完成（AI-2c 出过事）
4. 疑问与偏差：哪里没按任务卡做、为什么
5. 遗留问题：已知 bug、没做完的地方、建议

没有实际跑过的东西，不能写成已完成。
