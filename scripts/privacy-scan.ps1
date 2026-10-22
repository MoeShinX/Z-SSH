# 公开前的隐私扫描：对"要放进公开仓库的每一个文件"（含二进制）查找开发机的个人信息和密钥。
#
# 用法（在仓库根目录）：
#   powershell -File scripts/privacy-scan.ps1                 # 扫描 git 跟踪的 + 未被忽略的新文件（排除不公开的目录）
#   powershell -File scripts/privacy-scan.ps1 -Root <目录>     # 扫描某个目录下的所有文件（例如检出的 public 分支）
#   powershell -File scripts/privacy-scan.ps1 -SelfTest       # 自检：造一个带"假敏感值"的文件，必须命中
#
# 敏感值全部在运行时从环境变量和当前系统读取，脚本里不写任何真实值：
#   当前 Windows 用户名和用户目录、计算机名、git 的 user.name / user.email、
#   ZSSH_TEST_* 变量里的值（服务器地址、用户名、密码、代理地址等）、
#   本机智能体配置里用户自己配的 MCP 服务名（读不了就当没有，只读不改）。
# 另外查固定样式：令牌 / 密钥 / 私钥头、公网 IPv4、非 noreply 的邮箱。
#
# 输出只写"文件:位置 + 类别"，不打印命中的内容（二进制文件写字节偏移）。有命中退出码 1。
# 故意写在测试里的假值：命中行上带 mock / fake / dummy / example / test 等字样的令牌样式不算。

param(
    [string]$Root = "",
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

# 不公开的东西：开发过程记录、被替换的旧清单。
$excludedPrefixes = @("docs/tasks/", "docs/reports/", "docs/M1-manual-test.md", ".claude/")

function Get-Needles {
    $needles = New-Object System.Collections.Generic.List[object]
    $add = {
        param([string]$kind, [string]$value)
        if ([string]::IsNullOrWhiteSpace($value)) { return }
        $v = $value.Trim()
        if ($v.Length -lt 3) { return }
        $needles.Add([pscustomobject]@{ Kind = $kind; Value = $v })
    }
    & $add "本机用户名" $env:USERNAME
    & $add "用户目录" $env:USERPROFILE
    & $add "计算机名" $env:COMPUTERNAME
    & $add "git 用户名" (& git config user.name 2>$null)
    & $add "git 邮箱" (& git config user.email 2>$null)
    foreach ($item in (Get-ChildItem Env: | Where-Object { $_.Name -like "ZSSH_TEST_*" -or $_.Name -like "ZSSH_SHOT_*" })) {
        $value = [string]$item.Value
        if ($item.Name -match "(PORT|FULL|E2E)$") { continue }
        if ($item.Name -match "SOCKS5|PROXY" -and $value -match "^(.+):\d+$") { $value = $Matches[1] }
        if ($value -in @("127.0.0.1", "localhost", "1", "0", "true", "false")) { continue }
        # 太通用的用户名（root、admin 之类）满篇都是，不当敏感值；其余照查。
        if ($item.Name -like "*_USER*" -and $value -in @("root", "admin", "administrator", "ubuntu", "user", "test", "ec2-user", "debian")) { continue }
        & $add ("环境变量 " + $item.Name) $value
    }
    # 用户自己配的 MCP 服务名：从常见智能体配置里读 key，读不了就跳过。
    $profileDir = $env:USERPROFILE
    if ($profileDir) {
        $jsonFiles = @((Join-Path $profileDir ".claude.json"), (Join-Path $profileDir ".config\opencode\opencode.json"), (Join-Path $profileDir ".claude\settings.json"))
        foreach ($file in $jsonFiles) {
            try {
                if (-not (Test-Path -LiteralPath $file)) { continue }
                $json = Get-Content -LiteralPath $file -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
                foreach ($prop in @("mcpServers", "mcp")) {
                    $node = $json.$prop
                    if ($node) { foreach ($name in $node.PSObject.Properties.Name) { & $add "MCP 服务名" $name } }
                }
            } catch { }
        }
        $toml = Join-Path $profileDir ".codex\config.toml"
        try {
            if (Test-Path -LiteralPath $toml) {
                foreach ($m in [regex]::Matches((Get-Content -LiteralPath $toml -Raw), '(?m)^\[mcp_servers\.([^\].]+)')) { & $add "MCP 服务名" $m.Groups[1].Value }
            }
        } catch { }
    }
    return @($needles | Sort-Object Kind, Value -Unique)
}

# 固定样式。正则在 Latin1 字节串上跑，所以二进制文件也能扫。
$patterns = @(
    @{ Kind = "令牌样式 ghp_";        Re = 'ghp_[A-Za-z0-9]{8,}';                  Fakeable = $true },
    @{ Kind = "令牌样式 github_pat_"; Re = 'github_pat_[A-Za-z0-9_]{8,}';          Fakeable = $true },
    @{ Kind = "密钥样式 AKIA";        Re = 'AKIA[0-9A-Z]{12,}';                    Fakeable = $true },
    @{ Kind = "私钥头";               Re = '-----BEGIN [A-Z ]*PRIVATE KEY';        Fakeable = $true },
    @{ Kind = "密钥样式 sk-";         Re = '(?<![A-Za-z0-9])sk-[A-Za-z0-9]{16,}';  Fakeable = $true },
    @{ Kind = "邮箱";                 Re = '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'; Fakeable = $false }
)
$ipv4 = '(?<![0-9.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?![0-9.])'
$fakeMarker = 'mock|fake|dummy|example|not[_-]?a[_-]?real|placeholder|redact|sample|\bxxx|test'

function Test-PublicIpv4([string]$a, [string]$b, [string]$c, [string]$d) {
    $o = @([int]$a, [int]$b, [int]$c, [int]$d)
    foreach ($x in $o) { if ($x -gt 255) { return $false } }
    if ($o[0] -eq 10 -or $o[0] -eq 127 -or $o[0] -eq 0 -or $o[0] -ge 224) { return $false }
    if ($o[0] -eq 192 -and $o[1] -eq 168) { return $false }
    if ($o[0] -eq 172 -and $o[1] -ge 16 -and $o[1] -le 31) { return $false }
    if ($o[0] -eq 169 -and $o[1] -eq 254) { return $false }
    # 文档约定的示例地址
    if ($o[0] -eq 192 -and $o[1] -eq 0 -and $o[2] -eq 2) { return $false }
    if ($o[0] -eq 198 -and $o[1] -eq 51 -and $o[2] -eq 100) { return $false }
    if ($o[0] -eq 203 -and $o[1] -eq 0 -and $o[2] -eq 113) { return $false }
    return $true
}

function Get-FileList {
    if ($Root) {
        $base = (Resolve-Path $Root).Path
        return @(Get-ChildItem -LiteralPath $base -Recurse -File -Force | Where-Object { $_.FullName -notmatch '[\\/]\.git[\\/]' } | ForEach-Object {
            [pscustomobject]@{ Rel = $_.FullName.Substring($base.Length).TrimStart('\', '/').Replace('\', '/'); Full = $_.FullName }
        })
    }
    Push-Location $repo
    try { $names = & git ls-files --cached --others --exclude-standard } finally { Pop-Location }
    $out = @()
    foreach ($n in $names) {
        $skip = $false
        foreach ($p in $excludedPrefixes) { if ($n.StartsWith($p)) { $skip = $true; break } }
        if ($skip) { continue }
        $full = Join-Path $repo $n
        if (Test-Path -LiteralPath $full -PathType Leaf) { $out += [pscustomobject]@{ Rel = $n; Full = $full } }
    }
    return $out
}

function Get-Position([string]$hay, [int]$index, [bool]$isBinary) {
    if ($isBinary) { return "字节偏移 $index" }
    $line = 1 + ([regex]::Matches($hay.Substring(0, $index), "`n")).Count
    return "第 $line 行"
}

function Get-LineAt([string]$hay, [int]$index) {
    $start = $hay.LastIndexOf("`n", [Math]::Max(0, $index - 1)) + 1
    $end = $hay.IndexOf("`n", $index); if ($end -lt 0) { $end = $hay.Length }
    return $hay.Substring($start, $end - $start)
}

function Invoke-Scan($files, $needles) {
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $hits = New-Object System.Collections.Generic.List[string]
    foreach ($f in $files) {
        if ($f.Rel -match '(^|/)\.claude/|\.log$|(^|/)\.env' -and $f.Rel -notmatch '\.env\.example$') {
            $hits.Add(("{0}  [不该进仓库的文件]" -f $f.Rel))
        }
        $bytes = [System.IO.File]::ReadAllBytes($f.Full)
        $isBinary = $false
        $probe = [Math]::Min($bytes.Length, 8000)
        for ($i = 0; $i -lt $probe; $i++) { if ($bytes[$i] -eq 0) { $isBinary = $true; break } }
        $text = $latin1.GetString($bytes)
        # 文本文件多半是 UTF-8（中文用户名按 UTF-8 再看一遍）；二进制文件再按 UTF-16 看一遍。
        $views = @($text)
        if (-not $isBinary) { $views += [System.Text.Encoding]::UTF8.GetString($bytes) }
        else { $views += [System.Text.Encoding]::Unicode.GetString($bytes) }
        $seen = @{}
        foreach ($view in $views) {
            foreach ($n in $needles) {
                $re = '(?<![A-Za-z0-9])' + [regex]::Escape($n.Value) + '(?![A-Za-z0-9])'
                foreach ($m in [regex]::Matches($view, $re, 'IgnoreCase')) {
                    $pos = Get-Position $view $m.Index $isBinary
                    $key = "$($n.Kind)|$pos"
                    if ($seen[$key]) { continue }
                    $seen[$key] = $true
                    $hits.Add(("{0}:{1}  [{2}]" -f $f.Rel, $pos, $n.Kind))
                }
            }
        }
        # WOFF2 字体是 Brotli 压缩过的数据：随机字节会碰巧凑成"邮箱 / IP"的样子，按格式匹配没有意义
        # （里面的文字都压缩了，真有敏感内容也看不出来）。具体的敏感值（上面的 $needles）照常查。
        $shapeScan = $f.Rel -notmatch '\.woff2$'
        foreach ($p in $patterns) {
            if (-not $shapeScan) { break }
            foreach ($m in [regex]::Matches($text, $p.Re)) {
                $ln = Get-LineAt $text $m.Index
                if ($p.Fakeable -and $ln -match $fakeMarker) { continue }
                if ($p.Kind -eq "邮箱" -and ($m.Value -match 'noreply|@example\.|@([A-Za-z0-9-]+\.)+example\.(com|net|org)$|@localhost|\.example$|\.internal$|@openssh\.com$|\.(png|jpe?g|gif|svg|ico|webp|icns)$')) { continue }
                if ($p.Kind -eq "邮箱" -and $f.Rel -match '(^|/)(package-lock\.json|Cargo\.lock)$') { continue }
                $pos = Get-Position $text $m.Index $isBinary
                $key = "$($p.Kind)|$pos"
                if ($seen[$key]) { continue }
                $seen[$key] = $true
                $hits.Add(("{0}:{1}  [{2}]" -f $f.Rel, $pos, $p.Kind))
            }
        }
        # SVG 的路径数据是一长串小数（四段小数连在一起很常见），按 IP 的样子查全是误报；具体的敏感值照常查。
        $ipScan = $shapeScan -and $f.Rel -notmatch '\.svg$'
        foreach ($m in [regex]::Matches($text, $ipv4)) {
            if (-not $ipScan) { break }
            if (Test-PublicIpv4 $m.Groups[1].Value $m.Groups[2].Value $m.Groups[3].Value $m.Groups[4].Value) {
                $pos = Get-Position $text $m.Index $isBinary
                $key = "ip|$pos"
                if ($seen[$key]) { continue }
                $seen[$key] = $true
                $hits.Add(("{0}:{1}  [公网 IPv4 样式]" -f $f.Rel, $pos))
            }
        }
    }
    return $hits
}

$needles = Get-Needles
if ($SelfTest) {
    # 造一个带"运行时才拼出来的假敏感值"的文件：用户名、令牌样式、公网 IP、邮箱各一个，外加一个二进制文件。
    $dir = Join-Path ([System.IO.Path]::GetTempPath()) ("zssh-privacy-selftest-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force $dir | Out-Null
    try {
        $userNeedle = ($needles | Where-Object { $_.Kind -eq "本机用户名" } | Select-Object -First 1).Value
        $lines = @(
            ("owner=" + $userNeedle),
            ("t=" + "ghp" + "_" + ("A1b2C3d4" * 3)),
            ("ip=" + ((8, 8, 4, 4) -join ".")),
            ("mail=" + "someone" + "@" + "corp-mail.net")
        )
        [System.IO.File]::WriteAllLines((Join-Path $dir "sample.txt"), $lines)
        $bin = [byte[]](0, 1, 2) + [System.Text.Encoding]::ASCII.GetBytes($userNeedle) + [byte[]](0, 3)
        [System.IO.File]::WriteAllBytes((Join-Path $dir "sample.bin"), $bin)
        $files = @(Get-ChildItem -LiteralPath $dir -File | ForEach-Object { [pscustomobject]@{ Rel = $_.Name; Full = $_.FullName } })
        $hits = Invoke-Scan $files $needles
        $kinds = @($hits | ForEach-Object { ($_ -split '\[')[-1].TrimEnd(']') })
        $need = @("令牌样式 ghp_", "公网 IPv4 样式", "邮箱", "本机用户名")
        $missing = @($need | Where-Object { $kinds -notcontains $_ })
        $binHit = @($hits | Where-Object { $_ -like "sample.bin:*" }).Count
        if ($missing.Count -gt 0 -or $binHit -eq 0) {
            Write-Host ("自检失败：没有命中 " + ($missing -join "、") + $(if ($binHit -eq 0) { "；二进制文件里的用户名也没命中" } else { "" })) -ForegroundColor Red
            exit 1
        }
        Write-Host ("自检通过：4 类假值和二进制文件里的用户名都命中了（{0} 条）" -f $hits.Count) -ForegroundColor Green
        exit 0
    } finally { Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue }
}

$files = Get-FileList
Write-Host ("扫描 {0} 个文件；敏感值来源 {1} 项（类别：{2}）" -f $files.Count, $needles.Count, ((($needles | ForEach-Object { $_.Kind }) | Sort-Object -Unique) -join "、"))
$hits = Invoke-Scan $files $needles
if ($hits.Count -eq 0) {
    Write-Host "没有命中。" -ForegroundColor Green
    exit 0
}
Write-Host ("命中 {0} 处（只列文件、位置、类别，不显示内容）：" -f $hits.Count) -ForegroundColor Red
$hits | Sort-Object | ForEach-Object { Write-Host "  $_" }
exit 1
