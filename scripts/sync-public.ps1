# 把 main 的内容同步到本地 public 分支（公开仓库用的那条线）。
#
# 做什么：
#   1. 取 main 当前的文件树，去掉不公开的东西（docs/tasks、docs/reports、docs/M1-manual-test.md、.claude、截图和测试脚本、候选图标），
#      得到"应该公开的树"；
#   2. 和 public 分支现在的树比较，没有差别就什么都不做；
#   3. 有差别：在 public 上新建一个提交（父提交是 public 现在的头），作者和提交者都是
#      MoeShinX <MoeShinX@users.noreply.github.com>；
#   4. 提交**更新分支之前**先把这棵树导出来跑 scripts/privacy-scan.ps1，有命中就不更新 public、退出码 1；
#   5. 只动本地的 public 分支引用，不切换分支、不动工作区和暂存区、不推送、不碰任何远端。
#
# 用法（在仓库根目录）：
#   powershell -File scripts/sync-public.ps1                   # 同步
#   powershell -File scripts/sync-public.ps1 -DryRun           # 只看会改什么，不提交
#   powershell -File scripts/sync-public.ps1 -Message "概括"    # 提交说明第一行（默认"同步 N 个提交"）；正文总会列出每个源提交的标题
#   powershell -File scripts/sync-public.ps1 -Source <分支或提交>  # 不是同步 main，而是别的提交
#
# 同步完以后想推送，请自己确认后手动 git push（这个脚本不会推送）。
param(
    [string]$Source = "main",
    [string]$Branch = "public",
    [string]$Message = "",
    [switch]$DryRun,
    # 仓库根目录；默认是脚本所在仓库。测试用它指向临时仓库。
    [string]$RepoRoot = ""
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
if ($RepoRoot) { Set-Location $RepoRoot } else { Set-Location (Join-Path $PSScriptRoot "..") }

$authorName = "MoeShinX"
$authorEmail = "MoeShinX@users.noreply.github.com"
# 不公开的路径（目录用前缀，文件写全）。
# 测试/截图脚本、没采用的候选图标也不公开。正在用的图标是 tauri.conf.json 引用的 zssh-z-neon，不能排除。
$excluded = @(
    "docs/tasks", "docs/reports", "docs/M1-manual-test.md", ".claude",
    "scripts/screenshots", "scripts/test-sync-public.ps1",
    "src-tauri/icons/zssh-brand-v1", "src-tauri/icons/zssh-z"
)

function Invoke-Git {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$GitArgs)
    $out = & git @GitArgs 2>&1
    if ($LASTEXITCODE -ne 0) { throw "git $($GitArgs -join ' ') 失败：$out" }
    return $out
}

$sourceCommit = (Invoke-Git rev-parse --verify "$Source^{commit}") | Select-Object -First 1
$publicHead = (Invoke-Git rev-parse --verify "refs/heads/$Branch^{commit}") | Select-Object -First 1
$publicTree = (Invoke-Git rev-parse "$publicHead^{tree}") | Select-Object -First 1

# 1. 用临时的暂存区文件算出"应该公开的树"，不碰真正的暂存区。
$tmpIndex = Join-Path ([System.IO.Path]::GetTempPath()) ("zssh-sync-public-index-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$exportDir = Join-Path ([System.IO.Path]::GetTempPath()) ("zssh-sync-public-export-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$env:GIT_INDEX_FILE = $tmpIndex
try {
    Invoke-Git read-tree $sourceCommit | Out-Null
    foreach ($path in $excluded) {
        # 出错不能吞掉：以前只靠后面的隐私扫描兜底，git rm 失败时会把不该公开的树提交进 public。
        $rmError = & git rm -r --cached -q --ignore-unmatch -- $path 2>&1
        if ($LASTEXITCODE -ne 0) { throw "git rm $path 失败：$rmError" }
    }
    $newTree = (Invoke-Git write-tree) | Select-Object -First 1
} finally {
    Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue
    Remove-Item $tmpIndex -ErrorAction SilentlyContinue
}

if ($newTree -eq $publicTree) {
    Write-Host "public 已经是最新的（和 $Source 去掉不公开内容之后的树完全一样），没有要同步的。" -ForegroundColor Green
    exit 0
}

# 2. 会改什么
Write-Host "将要同步到 $Branch 的改动（相对于 $Branch 现在的头 $($publicHead.Substring(0, 7))）：" -ForegroundColor Cyan
& git diff --stat --no-color $publicTree $newTree
if ($DryRun) {
    Write-Host "（-DryRun：不提交。）" -ForegroundColor Yellow
    exit 0
}

# 3. 提交说明：第一行是概括（-Message 传入；没传就写"同步 N 个提交"），正文逐行列出这次包含的
#    每个源提交的标题（只算碰了公开路径的，纯文档提交不列），最后是 Synced-From。
#    "这次包含哪些"= 从 public 上一次记录的 Synced-From 到现在的 Source 之间的提交。
$pathspec = @(".") + ($excluded | ForEach-Object { ":(exclude)$_" })
$lastSynced = $null
foreach ($line in (& git log $publicHead --format=%B 2>$null)) {
    if ($line -match '^Synced-From:\s*([0-9a-f]{7,40})\s*$') { $lastSynced = $Matches[1]; break }
}
$range = $sourceCommit
if ($lastSynced) {
    & git merge-base --is-ancestor $lastSynced $sourceCommit 2>$null
    if ($LASTEXITCODE -eq 0) { $range = "$lastSynced..$sourceCommit" }
}
$subjects = @(& git log --reverse --format=%s $range -- @pathspec 2>$null | Where-Object { $_ })
if ($range -eq $sourceCommit -and $subjects.Count -gt 1) {
    # 找不到上一次同步的位置：不把整个历史都列出来，只列最近一个。
    $subjects = @($subjects[-1])
}
if (-not $Message) {
    $Message = if ($subjects.Count -gt 0) { "同步 $($subjects.Count) 个提交" } else { "同步 $Source" }
}
$body = ($subjects | ForEach-Object { "- $_" }) -join "`n"
$fullMessage = if ($body) { "$Message`n`n$body`n`nSynced-From: $sourceCommit" } else { "$Message`n`nSynced-From: $sourceCommit" }

$env:GIT_AUTHOR_NAME = $authorName; $env:GIT_AUTHOR_EMAIL = $authorEmail
$env:GIT_COMMITTER_NAME = $authorName; $env:GIT_COMMITTER_EMAIL = $authorEmail
# 提交说明走文件（-F）而不是 -m：源提交的标题里可能有双引号（例如 git revert 生成的
# Revert "…"），PowerShell 5 把它当参数传给原生程序时会拆坏，commit-tree 就收到多余的参数。
$messageFile = [System.IO.Path]::GetTempFileName()
[System.IO.File]::WriteAllText($messageFile, $fullMessage, (New-Object System.Text.UTF8Encoding($false)))
try {
    $newCommit = (& git commit-tree $newTree -p $publicHead -F $messageFile 2>&1) | Select-Object -First 1
    if ($LASTEXITCODE -ne 0) { throw "git commit-tree 失败：$newCommit" }
} finally {
    Remove-Item Env:GIT_AUTHOR_NAME, Env:GIT_AUTHOR_EMAIL, Env:GIT_COMMITTER_NAME, Env:GIT_COMMITTER_EMAIL -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $messageFile -ErrorAction SilentlyContinue
}

# 4. 更新分支之前先扫描：把这个提交的树导出到临时目录，跑隐私扫描。
New-Item -ItemType Directory -Force $exportDir | Out-Null
try {
    # 用临时暂存区 + checkout-index 导出（不经过管道，二进制文件不会被改坏）。
    $exportIndex = Join-Path ([System.IO.Path]::GetTempPath()) ("zssh-sync-public-index2-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    $env:GIT_INDEX_FILE = $exportIndex
    try {
        Invoke-Git read-tree $newCommit | Out-Null
        Invoke-Git "--work-tree=$exportDir" checkout-index -a -f | Out-Null
    } finally {
        Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue
        Remove-Item $exportIndex -ErrorAction SilentlyContinue
    }
    # 提交说明也扫一遍（在 main 上写的说明可能带个人信息）
    Set-Content -Path (Join-Path $exportDir ".sync-commit-message.txt") -Value $fullMessage -Encoding UTF8
    & powershell -NoProfile -File (Join-Path $PSScriptRoot "privacy-scan.ps1") -Root $exportDir
    $scanExit = $LASTEXITCODE
} finally {
    Remove-Item -Recurse -Force $exportDir -ErrorAction SilentlyContinue
}
if ($scanExit -ne 0) {
    Write-Host "隐私扫描有命中，没有更新 $Branch（提交对象 $newCommit 留在仓库里但没有分支指向它，会被自动清理）。" -ForegroundColor Red
    exit 1
}

# 5. 扫描通过：只移动本地的分支引用（旧值作为保护，别人同时动过就失败）。
Invoke-Git update-ref "refs/heads/$Branch" $newCommit $publicHead | Out-Null
$check = (& git log -1 --format="%h  作者 %an <%ae>  提交者 %cn <%ce>" $Branch) | Select-Object -First 1
Write-Host "已更新本地 $Branch：$check" -ForegroundColor Green
Write-Host "没有推送。确认无误后请自己 git push（例如 git push origin ${Branch}:main）。"
exit 0
