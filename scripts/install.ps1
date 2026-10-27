# 安装 llm-wiki 二进制与 6 个 Claude Code skill（Windows PowerShell）。
#
#   .\scripts\install.ps1               构建 release 二进制并安装
#   .\scripts\install.ps1 -SkipBuild    用已构建的 target\release\llm-wiki.exe
#   .\scripts\install.ps1 -DryRun       只打印将要做什么，不动文件
#
# 可覆盖的环境变量：
#   CLAUDE_SKILLS_DIR  skill 安装目录，默认 $env:USERPROFILE\.claude\skills
#   WIKI_BIN_DIR       二进制安装目录，默认 $env:LOCALAPPDATA\Programs\llm-wiki

[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

$RepoDir    = Split-Path -Parent $PSScriptRoot
$SkillsSrc  = Join-Path $RepoDir 'skills'
$SkillsDst  = if ($env:CLAUDE_SKILLS_DIR) { $env:CLAUDE_SKILLS_DIR } else { Join-Path $env:USERPROFILE '.claude\skills' }
$BinDst     = if ($env:WIKI_BIN_DIR) { $env:WIKI_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\llm-wiki' }
$BinName    = 'llm-wiki.exe'
$BinSrc     = Join-Path $RepoDir "target\release\$BinName"

function Invoke-Step {
    param([scriptblock]$Action, [string]$Text)
    if ($DryRun) {
        Write-Host "  [dry-run] $Text"
    } else {
        & $Action
    }
}

# 1. 校验 skill 源目录
if (-not (Test-Path $SkillsSrc)) {
    Write-Error "找不到 $SkillsSrc，请在仓库根目录运行"
    exit 2
}

$names = Get-ChildItem -Path $SkillsSrc -Directory | Sort-Object Name | Select-Object -ExpandProperty Name
if ($names.Count -eq 0) {
    Write-Error "$SkillsSrc 下没有 skill 目录"
    exit 2
}

foreach ($n in $names) {
    if ($n -notmatch '^[a-z0-9-]+$') {
        Write-Error "skill 目录名不合法：$n"
        exit 2
    }
    if (-not (Test-Path (Join-Path $SkillsSrc "$n\SKILL.md"))) {
        Write-Error "缺少 $SkillsSrc\$n\SKILL.md"
        exit 2
    }
}

# 2. 构建二进制
if ($SkipBuild) {
    if (-not (Test-Path $BinSrc)) {
        Write-Error "没有 $BinSrc，去掉 -SkipBuild 再跑"
        exit 2
    }
} else {
    Write-Host "==> 构建 release 二进制"
    Invoke-Step { cargo build --release --locked --manifest-path (Join-Path $RepoDir 'Cargo.toml') } 'cargo build --release --locked'
}

# 3. 安装二进制
Write-Host "==> 安装 $BinName 到 $BinDst"
Invoke-Step { New-Item -ItemType Directory -Force -Path $BinDst | Out-Null } "mkdir $BinDst"
Invoke-Step { Copy-Item -Force -Path $BinSrc -Destination (Join-Path $BinDst $BinName) } "copy $BinName -> $BinDst"

# 4. 安装 skill（先删旧目录，避免残留已删除的子文件）
Write-Host "==> 安装 $($names.Count) 个 skill 到 $SkillsDst"
Invoke-Step { New-Item -ItemType Directory -Force -Path $SkillsDst | Out-Null } "mkdir $SkillsDst"
foreach ($n in $names) {
    Write-Host "    - $n"
    $target = Join-Path $SkillsDst $n
    Invoke-Step { if (Test-Path $target) { Remove-Item -Recurse -Force $target } } "rm -rf $target"
    Invoke-Step { Copy-Item -Recurse -Force -Path (Join-Path $SkillsSrc $n) -Destination $target } "copy $n -> $target"
}

# 5. 收尾检查
Write-Host ''
$onPath = Get-Command 'llm-wiki' -ErrorAction SilentlyContinue
if ($onPath) {
    Write-Host "已就绪：$($onPath.Source)"
} else {
    Write-Host "二进制已装到 $BinDst，但不在 PATH 上。"
    Write-Host '把这一行加进 PowerShell profile（$PROFILE）后重开终端：'
    Write-Host "    `$env:PATH = `"$BinDst;`$env:PATH`""
}
Write-Host "skill 目录：$SkillsDst"
