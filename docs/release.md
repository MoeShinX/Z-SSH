# 发布流程与更新签名

这份文档给维护者看：怎么生成更新签名密钥、怎么配 GitHub、怎么发一个新版本。

## 1. 更新签名密钥（只做一次）

自动更新靠一对签名密钥保证安全：发布时用**私钥**给安装包签名，已安装的 Z-SSH 用写在程序里的**公钥**验签，验不过就拒绝安装。这是"更新签名"，和 Windows 的代码签名证书（让系统不弹"未知发布者"）是两回事。

### 生成

在**仓库外面**的目录里生成（私钥绝不能进仓库）。需要先装 tauri-cli（`cargo install tauri-cli --version "^2"`）：

```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\.zssh-release" | Out-Null
cargo tauri signer generate -w "$env:USERPROFILE\.zssh-release\updater.key"
```

命令会**让你输入一个密码**来保护私钥。密码请自己想一个足够长的，记在密码管理器里；不要把它写进任何文件、聊天记录或脚本。生成后有两个文件：

- `updater.key`：私钥（保密）
- `updater.key.pub`：公钥（可以公开）

### 把公钥放进程序

打开 `updater.key.pub`，把里面那一整行文字替换到 `src-tauri/tauri.conf.json` 里 `plugins.updater.pubkey` 的占位串 `REPLACE_WITH_UPDATER_PUBLIC_KEY`，提交。**占位串没换掉时**：程序里"检查更新"会提示"没有配置更新签名公钥"；发布工作流也会直接失败，不会发出一个更新不了的版本。

### 把私钥交给 GitHub

在 GitHub 仓库的 Settings → Secrets and variables → Actions 里新建两个 Repository secret：

| 名称 | 内容 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `updater.key` 文件的**全部内容**（用记事本打开复制，不是路径） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 生成时设的密码 |

### 备份，以及丢了 / 泄露了会怎么样

**务必把 `updater.key` 和它的密码各备份一份到安全的地方**（例如加密的 U 盘、密码管理器的附件）。

- **私钥或密码丢了**：以后发布的新版本就没法签名，而**已经安装的用户手里的程序只认旧公钥**，所以他们**再也收不到自动更新**，只能手动下载新安装包重装（重装不会丢数据）。要恢复只能换一对新密钥、把新公钥发在一个新版本里，让用户手动装这一版；之后的版本才能重新自动更新。
- **私钥泄露了（或密码被别人知道）**：拿到私钥的人可以做出一个"看起来合法"的更新包，再配合劫持更新地址（或拿到你的 GitHub 账号）就能给你的用户推送恶意程序。发现泄露要立刻：撤销 GitHub 上的 Secret、停止使用这把密钥、换新密钥并通知用户手动装新版（旧密钥签过名的更新在用户那边仍然有效，所以越早换越好）。

## 2. 发一个新版本

1. 改版本号：`powershell -File scripts/bump-version.ps1 0.2.0`（同时改 `Cargo.toml`、`src-tauri/tauri.conf.json`、`ui/package.json`，并检查三处一致）。
2. 在 `CHANGELOG.md` 最上面加一节 `## [0.2.0] - 日期`，写这个版本的变化；预发布的版本在标题后面写"预发布"。**Release 的说明直接取自这一节。**程序里的更新弹窗会按 `### 新增` / `### 改进` / `### 修复` / `### 安全` 分组、每条编号显示，所以只用这几种小标题和 `- ` 条目；每条尽量一行说清楚，需要点名功能时写成 `- **功能名**：说明`。
3. 跑本地的隐私扫描和五条门禁：`powershell -File scripts/privacy-scan.ps1`（要求没有命中），再跑门禁。
4. 提交并推送，紧接着打标签并推送：`git tag v0.2.0` → `git push origin v0.2.0`。不用等 CI：CI（五条门禁）和 Release 工作流同时跑，Release 建出来的只是草稿，用户看不到。
5. GitHub Actions 的 Release 工作流自动构建、签名，并建成**草稿** Release（附安装包、`.sig` 签名文件、`latest.json` 和免安装 zip）。工作流分两个任务：**构建**任务取代码、装依赖、构建（npm 和 cargo 的构建脚本都在这里跑），拿不到签名私钥、只有只读权限；**签名**任务不取代码、不跑项目里的任何脚本，只装固定版本的 tauri-cli（npm 上官方预编译的 `@tauri-apps/cli`，装 `--ignore-scripts`，几秒装好；以前 `cargo install` 每个任务要从源码编 5～8 分钟），用 `tauri signer sign --app-version` 给安装包签名，再生成 `latest.json`、建草稿 Release，私钥只在这一步的环境变量里。发布不恢复 Actions 缓存，第三方 Action 固定到提交 SHA，tauri-cli 固定到确切版本（两个任务里都有，升级时一起改）。工作流会检查：标签和三处版本号一致、CHANGELOG 里有这一节、公钥不是占位串、安装包里带 `zssh-mcp.exe`、签名里带这个版本号。
6. **等这次提交的 CI 也变绿**，再到 GitHub 的 Releases 页面打开草稿，确认说明和附件没问题，点 **Publish**。CI 没过就不发布：删掉草稿和标签（`git push origin :refs/tags/v0.2.0`），修好后重新打。**发布之前已安装的用户检查不到这个版本**（`latest.json` 指向 `releases/latest`，草稿不算）。
7. 预发布（pre-release）不会被 `releases/latest` 指到：想让用户通过自动更新收到它，要在发布时取消"pre-release"勾选。

## 2.5 OneDrive 登录用的 client ID（Azure 应用注册，只做一次）

同步设置里的 OneDrive 用“浏览器授权登录”。登录要有一个在微软 Azure（Microsoft Entra）里注册的应用，注册之后得到的**应用程序（客户端）ID**（一个形如 `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` 的 GUID）要编进发布的程序。这个 ID 是公共客户端的标识，**不是秘密**，公开了也没关系（没有客户端密钥，用 PKCE 保护），但不写进代码，而是放在 GitHub 仓库的 **Variables**（不是 Secrets）里，构建时编进去。

注册（用你自己的微软账号登录 Azure 门户；个人账号即可，不需要付费订阅）：

1. 打开 Azure 门户 <https://portal.azure.com>，搜索「**应用注册**」（App registrations），进去点「**新注册**」。名称填 `Z-SSH`——它会成为用户 OneDrive 里 `Apps` 目录下那个应用专用文件夹的名字。
2. 「支持的帐户类型」选「**仅个人 Microsoft 帐户**」。（Z-SSH 的授权地址固定用 `consumers`，只支持个人账号：微软的 OneDrive 权限文档写明 `Files.ReadWrite.AppFolder` 这个委托权限只对个人账号有效，对工作 / 学校账号的说法互相矛盾，所以不做。）
3. 「重定向 URI」：平台选「**公共客户端/本机（移动和桌面）**」，地址填 `http://localhost`（**不要**带端口、不要带路径）。微软对 localhost 回调地址匹配时忽略端口，所以注册这一个地址就够，Z-SSH 运行时用随机端口。点「注册」。
4. 进「**身份验证**」→「高级设置」→「**允许公共客户端流**」（Allow public client flows），把「启用以下移动和桌面流」设成「**是**」，保存。
5. 进「**API 权限**」→「添加权限」→「Microsoft Graph」→「**委托的权限**」，勾选 `Files.ReadWrite.AppFolder`、`User.Read`、`offline_access`（`User.Read` 注册时一般已经有了），点「添加权限」。个人账号的用户在登录时自己同意，不需要管理员同意。
6. 回到「**概述**」页，复制「**应用程序(客户端) ID**」。

交给 GitHub：仓库 Settings → Secrets and variables → Actions → **Variables** 选项卡 → New repository variable，名称 `ZSSH_ONEDRIVE_CLIENT_ID`，值填上一步复制的 ID。发布工作流会检查它：没配置只警告（这个版本的 OneDrive 登录按钮不可用）；配了但格式不对直接失败。

自己从源码编译：构建前设环境变量 `ZSSH_ONEDRIVE_CLIENT_ID`（PowerShell：`$env:ZSSH_ONEDRIVE_CLIENT_ID = "…"`），再 `cargo tauri build`。不设也能编，只是登录按钮不可用，用户在同步设置的「高级」里填自己的 client ID 才能登录。

联网测试（`scripts/live-tests.ps1`）要 `ZSSH_ONEDRIVE_CLIENT_ID` 和 `ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN`：后者用 `cargo run -p zssh-core --example onedrive_refresh_token` 手动授权一次得到（会在终端打印刷新令牌，**只放进环境变量，不要提交、不要贴到聊天里**）；没设置这两个变量，对应测试会跳过并打印原因。

## 3. 自动更新是怎么走的

- 程序每次启动后（设置里可关，关掉就只能手动检查）请求 `https://github.com/MoeShinX/Z-SSH/releases/latest/download/latest.json`，走「连接」→「软件网络」里设置的代理。
- 自动检查到新版本：后台静默下载并**验签通过**，等用户退出 Z-SSH 时安装（`RunEvent::Exit` 里，排在智能体和编辑器清理之后，`restart_after_install(false)`，装完不重新打开）；底部不提示，被跳过的版本不下载。手动点「检查更新」：弹出对话框，用户可以立即更新、稍后、跳过这个版本，下载好后底部提示重启；重启前如果有传输、终端里的 AI 命令或编辑器里没保存的修改，会先问一次。
- 安装是当前用户的 NSIS 安装，不需要管理员权限；点“重启安装”装完自动重新启动，退出时自动安装则装完不重新打开。
- 免安装版（zip）照常检查并提示新版本，但不下载、不安装：提示里只有“打开下载页”，让用户下载新 zip 覆盖。判断依据是程序目录里没有 NSIS 安装时写出的 `uninstall.exe`。
- 安全：更新地址必须是 https；包必须有合法签名；没配代理就是直连（不读系统代理）。
- 防降级：开启了 `requireSignedVersion`。签名的 trusted comment 里必须带版本号，且要和 `latest.json` 公布的版本一致，否则拒绝安装；不带版本号的旧格式签名也会被拒。签名里带不带 `version:x.y.z` 取决于 tauri-cli 版本：实测 2.11.4 不带、2.12.3 带，所以发布工作流把 tauri-cli 固定在 2.12.3，构建后还会检查签名带版本号。本机用 `cargo tauri signer sign` 手动签名时也要用 2.12.3 以上。

## 4. 同步到公开分支、固定的 Rust 版本

- **同步到 `public` 分支**：`powershell -File scripts/sync-public.ps1`。它取 `main` 当前的内容，去掉不公开的 `docs/tasks`、`docs/reports`、`docs/M1-manual-test.md`，在本地 `public` 分支上新建一个提交（作者和提交者都是 `MoeShinX <MoeShinX@users.noreply.github.com>`），提交之前先跑隐私扫描，有命中就不更新。只改本地分支，不切换分支、不推送；`-DryRun` 只看会改什么。推送请自己确认后手动执行。
- **安装器模板**：`src-tauri/windows/installer.nsi` 是 tauri-bundler 2.10.3（tauri-cli 2.12.3 用的打包器）NSIS 模板的副本，只改了首次安装的默认目录：`%LOCALAPPDATA%\Programs\Z-SSH`（模板原本是 `%LOCALAPPDATA%\Z-SSH`）；更新和重装沿用注册表里记的上次位置。安装器的目录页照常可以改。升级 tauri-cli 时要拿新版打包器的模板对比，把这处改动带过去。
- **图标**：应用、安装器、卸载器都用 `src-tauri/icons/zssh-z-neon/`（来源和做法见该目录的 README）。
- **Rust 版本**由仓库根目录的 `rust-toolchain.toml` 固定（本机、CI、发布工作流都按它安装）。要升级时改这一个文件，在本机跑通五条门禁后再提交。
