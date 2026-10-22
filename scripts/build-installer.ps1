# 打 Windows 安装包（NSIS）。先单独编出 zssh-mcp.exe（安装包把它放在 Z-SSH.exe 同一目录），
# 再 cargo tauri build（打包专用配置 tauri.bundle.conf.json 把它放进安装目录；开发构建不依赖它）（前端依赖要先在 ui 目录 npm ci 过）。产物在 target\release\bundle\nsis\，不进 git。
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo build --release -p zssh-mcp
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo tauri build --config src-tauri/tauri.bundle.conf.json
exit $LASTEXITCODE
