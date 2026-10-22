# 统一修改版本号：根 Cargo.toml 的 [workspace.package]（所有包用 version.workspace = true）、
# src-tauri/tauri.conf.json、ui/package.json（以及 ui/package-lock.json 里本包那一行）。
#
# 用法：
#   powershell -File scripts/bump-version.ps1 0.2.0          # 修改三处
#   powershell -File scripts/bump-version.ps1 -Check         # 只检查三处是否一致（退出码 0/1）
#   powershell -File scripts/bump-version.ps1 -Check v0.2.0  # 检查三处都等于这个版本（发布工作流用）
param(
    [Parameter(Position = 0)][string]$Version = "",
    [switch]$Check
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$cargoPath = Join-Path $root "Cargo.toml"
$confPath = Join-Path $root "src-tauri\tauri.conf.json"
$pkgPath = Join-Path $root "ui\package.json"
$lockPath = Join-Path $root "ui\package-lock.json"
$utf8 = New-Object System.Text.UTF8Encoding($false)

$Version = $Version.TrimStart("v")
if ($Version -ne "" -and $Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') {
    Write-Error "版本号格式不对：$Version（应为 1.2.3 或 1.2.3-beta.1）"; exit 2
}

function Read-Text($p) { [System.IO.File]::ReadAllText($p, $utf8) }
function Get-CargoVersion {
    $text = Read-Text $cargoPath
    $m = [regex]::Match($text, '(?ms)^\[workspace\.package\]\s*(?:[^\[]*?)^version\s*=\s*"([^"]+)"')
    if (-not $m.Success) { Write-Error "Cargo.toml 的 [workspace.package] 里找不到 version"; exit 2 }
    $m.Groups[1].Value
}
function Get-JsonVersion($p) {
    $m = [regex]::Match((Read-Text $p), '(?m)^\s*"version"\s*:\s*"([^"]+)"')
    if (-not $m.Success) { Write-Error "$p 里找不到 version"; exit 2 }
    $m.Groups[1].Value
}

# 其余的 Cargo 包必须用 version.workspace = true，不能自己带版本号。
$strayCrates = @()
foreach ($toml in @("crates\zssh-core\Cargo.toml", "crates\zssh-mcp\Cargo.toml", "src-tauri\Cargo.toml")) {
    $text = Read-Text (Join-Path $root $toml)
    if ($text -notmatch '(?m)^version\.workspace\s*=\s*true') { $strayCrates += $toml }
}

if ($Check) {
    $found = [ordered]@{
        "Cargo.toml (workspace.package)" = Get-CargoVersion
        "src-tauri/tauri.conf.json"      = Get-JsonVersion $confPath
        "ui/package.json"                = Get-JsonVersion $pkgPath
    }
    $bad = $false
    foreach ($k in $found.Keys) { Write-Host ("{0,-34} {1}" -f $k, $found[$k]) }
    if (($found.Values | Sort-Object -Unique).Count -ne 1) { Write-Host "三处版本号不一致" -ForegroundColor Red; $bad = $true }
    if ($Version -ne "" -and ($found.Values | Where-Object { $_ -ne $Version }).Count -gt 0) {
        Write-Host "版本号和要求的 $Version 不一致" -ForegroundColor Red; $bad = $true
    }
    if ($strayCrates.Count -gt 0) { Write-Host ("这些包没有用 version.workspace = true：" + ($strayCrates -join "、")) -ForegroundColor Red; $bad = $true }
    if ($bad) { exit 1 }
    Write-Host "版本号一致。" -ForegroundColor Green
    exit 0
}

if ($Version -eq "") { Write-Error "用法：bump-version.ps1 <版本>   或   bump-version.ps1 -Check [版本]"; exit 2 }
if ($strayCrates.Count -gt 0) { Write-Error ("这些包没有用 version.workspace = true：" + ($strayCrates -join "、")); exit 2 }

# 1. 根 Cargo.toml：只改 [workspace.package] 里的 version。
$text = Read-Text $cargoPath
$new = [regex]::Replace($text, '(?ms)(^\[workspace\.package\]\s*(?:[^\[]*?)^version\s*=\s*")[^"]+(")', ('${1}' + $Version + '${2}'), 1)
[System.IO.File]::WriteAllText($cargoPath, $new, $utf8)
# 2. tauri.conf.json、package.json：改第一处顶层 "version"。
foreach ($p in @($confPath, $pkgPath)) {
    $t = Read-Text $p
    $n = [regex]::Replace($t, '(?m)^(\s*"version"\s*:\s*")[^"]+(")', ('${1}' + $Version + '${2}'), 1)
    [System.IO.File]::WriteAllText($p, $n, $utf8)
}
# 3. package-lock.json：顶层和 packages[""] 里本包的版本（有的话）。
if (Test-Path $lockPath) {
    $t = Read-Text $lockPath
    $n = [regex]::Replace($t, '(?s)^(\{\s*"name"\s*:\s*"[^"]+"\s*,\s*"version"\s*:\s*")[^"]+(")', ('${1}' + $Version + '${2}'), 1)
    $n = [regex]::Replace($n, '(?s)("packages"\s*:\s*\{\s*""\s*:\s*\{\s*"name"\s*:\s*"[^"]+"\s*,\s*"version"\s*:\s*")[^"]+(")', ('${1}' + $Version + '${2}'), 1)
    [System.IO.File]::WriteAllText($lockPath, $n, $utf8)
}
Write-Host "版本号已改为 $Version。Cargo.lock 会在下次 cargo 命令时自动更新；记得在 CHANGELOG.md 里加一节。"
& powershell -NoProfile -File $PSCommandPath -Check $Version
exit $LASTEXITCODE
