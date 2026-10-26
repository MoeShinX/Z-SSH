# M1 设计：连接与终端

项目名 Z-SSH，仓库目录 `E:\ZCode\Z-ssh`。只做 Windows 10/11 x64。

## 1. M1 范围

**做：**

- 主机库：主机增删改、分组、搜索、快速连接（`user@host:port`）
- 凭据：密码、私钥（含口令）、ssh-agent（Windows OpenSSH agent / Pageant）
- 主机指纹校验：首次连接确认，指纹变化时阻断
- 网络：SOCKS5 / HTTP 代理、跳板机（可多级）
- 终端：标签页、分屏、同一主机开多个 shell、断线检测与重连
- 基础设置：字体、字号、配色、回滚行数、选中即复制、右键粘贴、快捷键

**不做（留给后续里程碑）：** SFTP（M2）、端口转发（M2）、备份（M3）、AI Agent（M4）、本地 PowerShell 终端、会话日志、GBK 等非 UTF-8 编码。

## 2. 技术栈

版本以 2026-10 的最新稳定版为准，搭骨架时锁定：

- **后端**：Rust stable、`tauri` 2.12、`tokio` 1.x、`russh` 0.64、`rusqlite` 0.40（bundled）、`tokio-socks` 0.5、`windows` 0.62（调 DPAPI）
- **前端**：React 19 + TypeScript + Vite、`@xterm/xterm` 6.0（加 fit、webgl、search、web-links、unicode11 插件）、Tailwind、zustand

`russh` 默认使用 `aws-lc-rs` 加密后端，它在 Windows 上编译要 CMake 和 NASM，本机都没装。所以一开始就关掉默认特性，改用 `ring`：

```toml
russh = { version = "0.64", default-features = false, features = ["ring", "rsa", "flate2"] }
```

## 3. 工程结构

```
Z-ssh/
├── Cargo.toml                  # workspace
├── crates/zssh-core/           # 纯 Rust，不依赖 Tauri，可单测，以后换原生 UI 也能复用
│   └── src/
│       ├── ssh/
│       │   ├── dialer.rs       # 拨号链：直连 / SOCKS5 / HTTP CONNECT / 跳板机
│       │   ├── connection.rs   # 连接 actor：一条 SSH 连接、多个通道
│       │   ├── auth.rs         # 认证顺序与交互式提问
│       │   ├── known_hosts.rs  # OpenSSH 格式读写与指纹比对
│       │   └── shell.rs        # shell 通道、流控、窗口大小
│       ├── store/
│       │   ├── db.rs           # SQLite 与迁移
│       │   ├── secrets.rs      # DPAPI 加解密
│       │   └── migrations/     # 0001_init.sql …
│       └── model.rs
├── src-tauri/                  # Tauri 外壳：命令、事件、全局状态
│   └── src/{main.rs, state.rs, commands/}
└── ui/                         # React 前端
    └── src/{api/, stores/, components/{sidebar,tabs,terminal,dialogs}}
```

原则：所有业务逻辑放在 `zssh-core`，`src-tauri` 只做命令和事件的转接。

## 4. 连接模型

### 4.1 一条连接、多个通道

- 每次连接产生一个 `Connection`：一条已认证的 SSH 连接，由一个 tokio 任务独占持有 `russh` 的 Handle（actor 模式）。外部通过 mpsc 发命令，通过事件拿状态。
- 在已连接的主机上"复制会话"或"分屏"时，直接在这条连接上再开一个 shell 通道。不用重新握手和认证，秒开，和 Xshell 的复制会话一样。
- 后面的 SFTP 和命令执行（M2/M3/M4）也作为同一连接上的通道。
- 引用计数：最后一个通道关闭后，空闲 60 秒再断开连接。
- **共用连接时的背压（已定，M1-06 实现）**：russh 的背压会卡住整条连接（见 5.2）。为了不让一个刷屏的终端拖住同一连接上的其他通道，做两级缓冲：
  1. 读取任务**始终**从 russh 读数据，放进该通道自己的本地缓冲，上限 8MB。
  2. 转发任务从本地缓冲取数据发给前端，受 FlowGate 控制（未确认超过 1MB 就停发）。
  3. 只有本地缓冲满了，读取任务才停止读 russh，这时才会卡整条连接。正常刷屏时前端渲染跟得上，碰不到这个上限。
  4. M2 的 SFTP 如果实测仍受影响，再单独开一条连接。

### 4.2 拨号链

一个 `Dialer` 只负责一件事：拿到一个能读写的字节流，然后交给 `russh::client::connect_stream`。

```
直连 TCP ─┐
SOCKS5 ───┼─> 跳板机1 (direct-tcpip) ─> 跳板机2 ─> 目标主机
HTTP ─────┘
```

- 代理：SOCKS5（可带用户名密码）、HTTP CONNECT
- 跳板机：先连上跳板机，开 `direct-tcpip` 通道连到下一跳，再在这个通道流上跑 SSH
- 链式组合：每一跳可以单独选代理。日常连服务器要经本地代理 `127.0.0.1:10808`，所以 M1 就必须有
- 连接超时 15 秒，每一跳单独计时，报错要写清是哪一跳失败

### 4.3 认证

按顺序尝试，任一种成功即停：

1. 主机配置的私钥（有口令时先查保险库，没有就弹窗问）
2. ssh-agent：先试 OpenSSH agent 的命名管道 `\\.\pipe\openssh-ssh-agent`，再试 Pageant。russh 在 Windows 上的 agent 支持要实测
3. 密码（保险库里有就用，没有就弹窗）
4. keyboard-interactive：服务器发来的每道题（比如 MFA 验证码）原样转给前端弹窗，答完回传

弹窗提问统一走"请求 ID + 事件 + 应答命令"：后端发 `auth_prompt { req_id, prompts }`，用 oneshot 等前端调 `auth_answer(req_id, answers)`。超过 120 秒没回答就算取消。

### 4.4 主机指纹

- 自己维护 `%APPDATA%\Z-ssh\known_hosts`，OpenSSH 格式。另外只读加载 `~/.ssh/known_hosts`，这样系统 ssh 已经信任过的主机不用再确认。
- **未知主机**：弹窗显示 SHA256 指纹和密钥类型，可选"信任并保存"或"仅本次"。
- **指纹变化**：红色警告，列出已保存的旧指纹和本次收到的新指纹。默认按钮是"关闭"（阻断）；用户明确点"更新指纹并连接"时，用新指纹替换自己 known_hosts 里的旧记录（`@revoked` 行保留），这次连接用保存的凭据继续，不用再去主机设置里清指纹、重输密码（用户 2026-10-26 的要求：服务器重装 / DD 后最常见）。"仅本次"不接受指纹变化。主机设置里的"清除已保存的指纹"照旧保留。
- **密钥被吊销**（`@revoked`）：只能关闭，不提供继续。
- 有跳板机时，每一跳都要校验。

### 4.5 保活与断线

- `russh` 配置 `keepalive_interval = 30s`、`keepalive_max = 3`，大约 90 秒内能发现断线。
- 断线后标签页变灰并提示原因，按 `R` 或点按钮重连。重连会在同一个标签页里开新的连接和 shell，旧的回滚内容保留。
- M1 不做自动重连，免得密码错误时反复重试被服务器封 IP。

## 5. 终端数据流（M1 最关键的部分）

### 5.1 输出：后端 → 前端

- 后端从 shell 通道读数据，**攒批**后再发给前端：每 16ms 或攒满 64KB 发一次，以先到者为准。避免每个小包都走一次 IPC。
- 用 Tauri 2 的 `ipc::Channel` 发原始字节，不用 event 也不用 JSON 字符串。Channel 是专门为流式数据设计的，吞吐量高得多。
- 前端调用 `term.write(bytes, callback)`。

### 5.2 流控（背压）

这是 `cat` 大文件、`yes`、刷屏日志时界面不卡死的关键。Netcatty 有大量修复提交集中在这里，M1 一开始就要做对。

- 前端在 `write` 的回调里累计已渲染的字节数，每渲染约 64KB 回传一次 `term_ack(session_id, bytes)`。
- 后端记录"已发送但未确认"的字节数：
  - 超过高水位（1MB）就**停止读通道**。
  - 降到低水位（256KB）以下再恢复读取。
- 验收标准：`cat` 一个 100MB 的文件时界面能操作，按 Ctrl+C 一秒内有反应。

**russh 的实际背压机制（M1-01 验收时核对过 russh 0.64.1 源码）：**

- russh 在**收到** `CHANNEL_DATA` 时就自动补窗口（`adjust_window_size`），不是等应用读走数据才补。所以 SSH 窗口本身拦不住服务器。
- 收到的数据通过一个有界队列（`Config::channel_buffer_size`，默认 100 条消息）交给通道。应用停止读取 → 队列塞满 → russh 的**整条连接事件循环**卡在投递上 → 不再读 TCP → TCP 层背压 → 服务器停。
- 结论：
  - 单连接单通道时，"停止读通道"能起到背压作用，M1-02 照此实现。
  - 暂停期间，同一连接上的其他通道（另一个 shell、SFTP）和 keepalive 也会一起卡住。M1-06 做"一条连接多个通道"时必须考虑，见 4.1。
  - Ctrl+C 的延迟等于排在它前面的数据量：我们自己未确认的字节、russh 队列（最多 100 个包，每包最多 32KB）、再加 TCP 缓冲。所以要把 `channel_buffer_size` 调小，并实测 Ctrl+C 延迟。
  - **硬规则：ack 和关闭不能排在任何可能阻塞的操作后面。**russh "句柄 → 事件循环"的队列只有 10 个位置，事件循环卡在投递数据时不会去读它。暂停期间如果连续写入（按住键、粘贴、频繁改窗口大小）超过 10 次，写操作就会阻塞。这时 ack 如果和写入排在同一个队列里，就永远轮不到，读取任务永远不恢复，形成死锁。所以 ack 和关闭要直接操作流控门（它是线程安全的），不经过会话的命令队列。
- M1-02 实测（链路 4～6MB/s）：Ctrl+C 到输出停止 0.45～0.5 秒，之后在途数据 1.3～1.6MB，大头是 russh 默认 2MB 的 SSH 接收窗口（`Config::window_size`）。链路很慢时可能超过 1 秒。**暂不调小 window_size**，因为它对所有通道生效，会拖慢 M2 的 SFTP。M2 时再统一权衡，可选办法是给终端和 SFTP 用不同的连接配置。

### 5.3 输入与窗口大小

- xterm 的 `onData` → `term_write(session_id, data)`。按键数据很小，直接走普通 command。
- 窗口大小：fit 插件算出行列数 → `term_resize(session_id, cols, rows)`，前端防抖 100ms → 通道的 `window_change`。
- 打开 shell 时请求 PTY，`TERM=xterm-256color`，初始行列数按 pane 的实际大小。
- 编码 M1 固定 UTF-8，用 xterm 自带的解码。GBK 放到后面，在后端转码。

## 6. 存储

### 6.1 SQLite

文件在 `%APPDATA%\Z-ssh\data.db`。M1 的表：

- `groups`：id、name、parent_id、sort
- `hosts`：id、group_id、name、host、port、username、auth_method、identity_id、proxy_id、jump_host_id、tags、sort、created_at、updated_at
- `identities`：id、name、kind（password / key_file / key_inline / agent）、key_path、secret_blob
- `proxies`：id、name、kind（socks5 / http）、host、port、username、secret_blob
- `settings`：key、value（JSON）
- `schema_version`

迁移按版本号递增执行。**写好的迁移不再改**，新增列只在新迁移里加，对应的索引也在新迁移里建。

### 6.2 敏感信息

- 密码、私钥口令和导入的私钥内容，用 **DPAPI**（`CryptProtectData`，绑定当前 Windows 用户）加密后存进 `secret_blob`。
- 不用 Windows 凭据管理器：它单条有大小上限，RSA 4096 私钥会超；而且敏感信息和配置分开存，迁移和备份都更麻烦。
- 私钥支持两种方式：引用磁盘上的文件路径，或者导入保险库。格式支持 OpenSSH 和 PEM，PuTTY 的 PPK 能顺手支持就做。
- 以后要导出配置给别的电脑用时，再加一层主密码加密（argon2 + XChaCha20）。M1 不做。

## 7. IPC 接口

**命令：**

- 主机库：`host_list`、`host_save`、`host_delete`、`group_save`、`group_delete`
- 凭据与代理：`identity_list` / `identity_save` / `identity_delete`，`proxy_list` / `proxy_save` / `proxy_delete`
- 会话：
  - `session_open(host_id | quick_target, output_channel) -> session_id`：**立即返回**会话 ID，连接在后台进行，进度和结果通过 `session_state` 事件通知；连接中调用 `session_close` 等于取消（M1-06 起）
  - `session_duplicate(session_id, output_channel) -> session_id`：复用连接
  - `term_write`、`term_resize`、`term_ack`、`session_close`、`session_reconnect`
- 交互应答：`hostkey_answer(req_id, decision)`、`auth_answer(req_id, answers)`
- 设置：`settings_get`、`settings_set`

**事件：**

- `session_state { session_id, state }`：state 依次是 connecting(hop) → authenticating → connected → disconnected(reason)
- `hostkey_prompt { req_id, host, key_type, fingerprint, changed }`
- `auth_prompt { req_id, kind, prompts }`

## 8. 界面

- **左侧栏**：分组树、搜索框（拼音首字母搜索可以后加）、顶部的快速连接输入框
- **主区**：标签页，每个标签内部是一棵分屏树，每个叶子是一个终端
- **分屏**：M1 用快捷键和右键菜单分屏，拖拽改大小；拖拽标签来分屏放到以后
- **主机编辑弹窗**：名称、分组、地址、端口、用户名、认证方式、凭据、代理、跳板机
- **默认快捷键**：
  - `Ctrl+Shift+T` 新标签，`Ctrl+Shift+W` 关闭
  - `Ctrl+Shift+D` 左右分屏，`Ctrl+Shift+E` 上下分屏
  - `Ctrl+Shift+C/V` 复制粘贴，`Ctrl+Shift+F` 搜索
  - `Ctrl+Tab` 切换标签
- **字体**：默认 Cascadia Mono（Win11 自带），可改
- **配色**：内置两三套
- **回滚行数**：默认 10000

## 9. 应用自身的安全

- Tauri capabilities 只开放自己定义的命令，不启用 fs、shell 等内置插件
- 严格 CSP，不加载任何远程内容
- 日志里不打印密码、口令、私钥和 keyboard-interactive 的回答

## 10. 验收清单

1. 密码、私钥、带口令私钥、agent、keyboard-interactive（MFA）各连通一台主机
2. 走 SOCKS5 `127.0.0.1:10808` 能连上；走一级、二级跳板机能连上
3. 未知主机弹窗；手动改 known_hosts 制造指纹变化，必须被阻断
4. vim、htop、tmux 显示正常；拉伸窗口后行列数正确；中文显示正常，中文输入法能输入
5. `cat` 100MB 文件时界面不卡，Ctrl+C 一秒内生效
6. 同一主机分屏秒开（复用连接，不重新认证）
7. 拔网线约 90 秒内标记断线，按 R 重连成功
8. 开 5 个终端，空闲时总内存（含 WebView2 进程）目标 150MB 以内，实测记录。口径是各进程**私有工作集**之和（任务管理器"内存"列）。M1-01 单会话基线是 99MB
9. 老服务器兼容：CentOS 7 这类只支持 `ssh-rsa` 和老算法的机器能连上（必要时在主机高级设置里放开旧算法）

`zssh-core` 的单元测试覆盖 known_hosts 解析和比对、拨号链组合（用模拟流测试）、迁移、DPAPI 加解密往返。

## 11. 实施顺序

每一步对应一张任务卡，按顺序执行，上一步验收通过再开下一步。

1. 搭骨架：workspace、Tauri 2、React、xterm 跑通；`zssh-core` 能直连密码主机，在前端看到 shell
2. 输出攒批、流控 ack、窗口大小同步（先把第 5 节做扎实）
3. SQLite、DPAPI、主机库和凭据的界面
4. 认证全流程、keyboard-interactive、known_hosts 弹窗
5. 拨号链：SOCKS5、HTTP、跳板机
6. 标签、分屏、复用连接、断线重连
7. 界面移植 Netcatty：主题、组件、布局、终端主题（2026-10-14 插入，原来的第 7 步顺延为第 8 步）
8. 设置页，按验收清单逐条过，然后由用户统一做手动测试

## 12. 风险

- **russh 兼容老服务器**：默认禁用了 SHA1 的 `ssh-rsa` 和一些老的 KEX 算法。主机高级设置里要能放开，并且要拿真实的老机器测。
- **russh 在 Windows 上的 agent / Pageant 支持**：要实测。不行的话 M1 先只做私钥文件，agent 往后放。
- **xterm WebGL 渲染**：个别显卡或远程桌面环境下可能有问题，要能自动降级到 DOM 渲染。
- **Tauri Channel 传二进制的吞吐**：第 2 步就压测。如果不够，改用自定义协议或本地 WebSocket。
