# 性能测试（不进五门禁）：跑被 #[ignore] 标记的吞吐/延迟门限测试。
#
# 这些测试依赖测试服务器所在链路的网速（刷屏吞吐、往返延迟门限），链路抖动会误报，
# 所以从 `cargo test --workspace` 里摘出来，需要专门跑并人读结果（卡片 AI-2b 第 5 节）。
#
# 用法（在仓库根目录）：
#   powershell -File scripts/perf-tests.ps1
#
# 前置：和别的 live 测试一样，需要 ZSSH_TEST_* 环境变量（缺了测试会自己跳过并打印原因）。
# 结果请抄进对应卡片报告；流控/缓冲/连接池相关的卡片必须跑一遍这个脚本。

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

Write-Host "== 性能测试（#[ignore]）开始 ==" -ForegroundColor Cyan
Write-Host "依赖网速的吞吐/延迟门限；链路慢时会失败，先确认网络再判代码问题。" -ForegroundColor Yellow

# --release 跑，别让 debug 构建的慢掩盖真实吞吐。
cargo test --release --workspace -- --ignored --nocapture
$code = $LASTEXITCODE

if ($code -eq 0) {
    Write-Host "== 性能测试通过 ==" -ForegroundColor Green
} else {
    Write-Host "== 性能测试失败（先看是不是链路慢）==" -ForegroundColor Red
}
exit $code
