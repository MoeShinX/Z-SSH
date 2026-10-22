# 完整联网测试（不进五门禁）：SFTP 全套、断线续传、AI 驱动、认证、代理/跳板、连接池、
# AI 文件工具等一切依赖测试服务器的用例。门禁里只留冒烟组（连接、开 shell、SFTP 列一次目录、传一个小文件）。
#
# 用法（在仓库根目录）：
#   powershell -File scripts/live-tests.ps1
#   powershell -File scripts/live-tests.ps1 -AgentE2E     # 同时跑三个智能体的端到端（真的调用模型）
#
# 前置：ZSSH_TEST_* 环境变量（缺了测试会自己跳过并打印原因）。
# -AgentE2E 等价于设 ZSSH_AGENT_E2E=1：opencode / codex / Claude Code 各做一次"读、改一行、写回"。
# -AgentE2E 还会跑 opencode_free_read_live：隔离目录里用真实 opencode 的免费模型，验证本机 read 工具一律被拒
# （没装 opencode 或没有可用的免费模型时打印「跳过」）。
# 本机 PATH 上的 opencode 低于 1.19 时（SQLite 外键 bug）那一项会跳过并提示；
# 用环境变量 ZSSH_AGENT_OPENCODE 指向 1.19 以上的 opencode 可执行文件即可。
#
# 服务器上只用 /tmp/zssh-sftp-live-* 专用临时目录（测试自己的守卫在任何退出路径都删）；
# 跑完本脚本再用 sftp_tmp_admin 核对剩余 0 个，有残留就让脚本失败。
# 吞吐/延迟门限类测试仍在 scripts/perf-tests.ps1。每张卡的报告都要贴本脚本的结果。

param([switch]$AgentE2E)

$ErrorActionPreference = "Continue"
Set-Location (Join-Path $PSScriptRoot "..")

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:ZSSH_LIVE_FULL = "1"
if ($AgentE2E) { $env:ZSSH_AGENT_E2E = "1" }
$log = Join-Path ([System.IO.Path]::GetTempPath()) "zssh-live-tests.log"
$watch = [System.Diagnostics.Stopwatch]::StartNew()
Write-Host "== 完整联网测试开始 ==" -ForegroundColor Cyan

# 系统临时目录里 zssh* 开头的目录和文件（.log 除外，脚本自己的日志）：开始和结束各数一次，多出来的就是测试没删干净的。
function Get-ZsshTempDirs {
    @(Get-ChildItem -LiteralPath ([System.IO.Path]::GetTempPath()) -Filter 'zssh*' -Force -ErrorAction SilentlyContinue | Where-Object { $_.Extension -ne '.log' } | ForEach-Object { $_.FullName })
}
$tempBefore = Get-ZsshTempDirs
Write-Host ("临时目录（%TEMP% 里 zssh* 目录和文件（.log 除外））开始时：{0} 个" -f $tempBefore.Count)

# --- S-2：配置同步的联网测试要一个真实的 WebDAV 实现 ---
# 本机临时起一个 wsgidav（Python 写的、独立于 Z-SSH 的真实 WebDAV 服务），跑完关掉、删数据。
# 已经设了 ZSSH_TEST_WEBDAV_URL（别人给的服务）就直接用它，不起本地的。
# 没有 python / 装不上 wsgidav 时跳过并打印原因（测试自己也会打印"跳过"），不算失败。
# S3 的联网测试读 ZSSH_TEST_S3_*，没设置就跳过（本机没有真实的 S3 兼容服务可用）。
# GitHub（Gist）的联网测试读 ZSSH_TEST_GITHUB_TOKEN（可选 ZSSH_TEST_GITHUB_API、ZSSH_TEST_GITHUB_SOCKS5），
# 没设置就跳过。设置了：测试新建一个临时的 secret Gist（描述里有随机串）跑完整同步，结束时删掉它
# （正常结束、断言失败、提前返回都会删），全程不列、不读、不改令牌所属账号里的其他 Gist。
$liveDav = $null
function Start-LiveWebDav {
    if ($env:ZSSH_TEST_WEBDAV_URL) {
        Write-Host "使用已设置的 ZSSH_TEST_WEBDAV_URL（不起本地 WebDAV）"
        return $null
    }
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) {
        Write-Host "跳过本地 WebDAV：没找到 python（同步的 WebDAV 联网测试会打印跳过）" -ForegroundColor Yellow
        return $null
    }
    $base = Join-Path ([System.IO.Path]::GetTempPath()) ("zssh-live-webdav-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    $root = Join-Path $base "root"
    New-Item -ItemType Directory -Force $root | Out-Null
    $venv = Join-Path $base "venv"
    & $python.Source -m venv $venv 2>&1 | Out-Null
    $venvPython = Join-Path $venv "Scripts\python.exe"
    & $venvPython -m pip install --quiet wsgidav cheroot 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "跳过本地 WebDAV：装不上 wsgidav（需要能访问 PyPI，可以设 HTTPS_PROXY）" -ForegroundColor Yellow
        Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
        return $null
    }
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
    $password = [guid]::NewGuid().ToString("N")
    $config = Join-Path $base "wsgidav.yaml"
    @"
host: 127.0.0.1
port: $port
provider_mapping:
  "/": "$($root.Replace('\', '/'))"
simple_dc:
  user_mapping:
    "*":
      "livetest":
        password: "$password"
http_authenticator:
  domain_controller: null
  accept_basic: true
  accept_digest: false
  default_to_digest: false
dir_browser:
  enable: false
verbose: 1
"@ | Set-Content -Encoding UTF8 $config
    $wsgidav = Join-Path $venv "Scripts\wsgidav.exe"
    $proc = Start-Process -FilePath $wsgidav -ArgumentList @("--config", $config) `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $base "out.log") -RedirectStandardError (Join-Path $base "err.log")
    $ready = $false
    for ($i = 0; $i -lt 50; $i++) {
        Start-Sleep -Milliseconds 300
        try { $c = [System.Net.Sockets.TcpClient]::new("127.0.0.1", $port); $c.Close(); $ready = $true; break } catch { }
    }
    if (-not $ready) {
        Write-Host "跳过本地 WebDAV：wsgidav 没有在 15 秒内起来" -ForegroundColor Yellow
        & taskkill /PID $proc.Id /T /F 2>&1 | Out-Null
        Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
        return $null
    }
    $env:ZSSH_TEST_WEBDAV_URL = "http://127.0.0.1:$port/"
    $env:ZSSH_TEST_WEBDAV_USER = "livetest"
    $env:ZSSH_TEST_WEBDAV_PASSWORD = $password
    Write-Host "本地 WebDAV（wsgidav）已启动：127.0.0.1:$port，数据目录 $root"
    return @{ Process = $proc; Base = $base; Root = $root }
}
$liveDav = Start-LiveWebDav
# OneDrive 的联网测试读 ZSSH_ONEDRIVE_CLIENT_ID 和 ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN（用 cargo run -p zssh-core --example
# onedrive_refresh_token 手动授权一次得到，只放环境变量），可选 ZSSH_TEST_ONEDRIVE_SOCKS5；缺一个就跳过。设置了：用刷新令牌换访问令牌，
# 在应用专用文件夹里读写一个带随机串的测试文件（条件写入冲突），结束时删掉（任何退出路径都删）；真正的同步包不碰。
if (-not $env:ZSSH_ONEDRIVE_CLIENT_ID -or -not $env:ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN) {
    Write-Host "OneDrive 联网测试：没设置 ZSSH_ONEDRIVE_CLIENT_ID 和 ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN，会跳过（刷新令牌要用户手动授权一次，见 docs/release.md）" -ForegroundColor Yellow
}
if (-not $env:ZSSH_TEST_GITHUB_TOKEN) {
    Write-Host "GitHub 联网测试：没设置 ZSSH_TEST_GITHUB_TOKEN，会跳过（设置后会新建临时 secret Gist 跑完整同步，结束时删除）" -ForegroundColor Yellow
}

# 联网测试共用服务器和临时目录，串行跑最稳。
# --no-fail-fast：一个文件失败了也把别的文件跑完，报告要看全貌。
cargo test --workspace --no-fail-fast -- --nocapture --test-threads=1 2>&1 | Tee-Object -FilePath $log
$code = $LASTEXITCODE

# 关掉自己起的 WebDAV，核对它的数据目录里没有残留，再删掉整个临时目录。
if ($liveDav) {
    $left = @(Get-ChildItem -Recurse -Force $liveDav.Root -ErrorAction SilentlyContinue).Count
    & taskkill /PID $liveDav.Process.Id /T /F 2>&1 | Out-Null
    Remove-Item -Recurse -Force $liveDav.Base -ErrorAction SilentlyContinue
    Remove-Item Env:ZSSH_TEST_WEBDAV_URL, Env:ZSSH_TEST_WEBDAV_USER, Env:ZSSH_TEST_WEBDAV_PASSWORD -ErrorAction SilentlyContinue
    Write-Host "-- 本地 WebDAV 已关闭，数据目录里残留 $left 个文件/目录；临时目录已删除 --"
    if ($left -ne 0) {
        Write-Host "本地 WebDAV 的数据目录里有测试残留！" -ForegroundColor Red
        if ($code -eq 0) { $code = 1 }
    }
}

# 汇总：把每个测试二进制的 "test result: ... N passed; M failed" 加起来。
$passed = 0; $failed = 0; $ignored = 0
foreach ($line in (Select-String -Path $log -Pattern '^test result: .*?(\d+) passed; (\d+) failed; (\d+) ignored')) {
    $m = $line.Matches[0]
    $passed += [int]$m.Groups[1].Value
    $failed += [int]$m.Groups[2].Value
    $ignored += [int]$m.Groups[3].Value
}
$skipped = (Select-String -Path $log -Pattern '跳过 [\w.]+').Count

# 服务器上不能留下测试的临时目录。
Write-Host "-- 核对服务器上没有残留的临时目录 --"
$leftover = cargo run -q -p zssh-core --example sftp_tmp_admin -- list 2>&1 | Out-String
Write-Host $leftover.Trim()
if ($leftover -notmatch '匹配 zssh-sftp-live-\* 的目录 0 个') {
    Write-Host "服务器上有测试临时目录残留！" -ForegroundColor Red
    if ($code -eq 0) { $code = 1 }
}

# 测试没删干净的临时目录：结束时多出来的算失败。
$tempAfter = Get-ZsshTempDirs
$tempNew = @($tempAfter | Where-Object { $tempBefore -notcontains $_ })
Write-Host ("临时目录结束时：{0} 个（多出 {1} 个）" -f $tempAfter.Count, $tempNew.Count)
if ($tempNew.Count -gt 0) {
    Write-Host "测试留下了没删的临时目录：" -ForegroundColor Red
    $tempNew | ForEach-Object { Write-Host "  $_" }
    if ($code -eq 0) { $code = 1 }
}

$watch.Stop()
Write-Host ("通过 {0}，失败 {1}，忽略 {2}，打印了「跳过」的 {3} 项" -f $passed, $failed, $ignored, $skipped)
Write-Host ("总耗时 {0:N1} 秒，退出码 {1}" -f $watch.Elapsed.TotalSeconds, $code)
if ($code -eq 0) {
    Write-Host "== 完整联网测试通过 ==" -ForegroundColor Green
} else {
    Write-Host "== 完整联网测试失败 ==" -ForegroundColor Red
}
exit $code
