<#
.SYNOPSIS
  Copies the package's prebuilt llm-wiki.exe onto PATH.

.DESCRIPTION
  Offline, no compiler, no administrator rights: it copies one file that
  already sits next to this script. Honours WIKI_BIN_DIR (default
  %LOCALAPPDATA%\Programs\llm-wiki) and leaves a working install alone unless
  -Force is given.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File bootstrap.ps1

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File bootstrap.ps1 -Force
#>
[CmdletBinding()]
param([switch]$Force)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

$triple = 'x86_64-pc-windows-msvc'
$src = Join-Path $here 'vendor\$triple\llm-wiki.exe'
if (-not (Test-Path -LiteralPath $src)) {
  Write-Error "package has no $triple binary (looked in $src). Run ``scripts/vendor.sh $triple <path-to-binary>`` where the package is built."
}

$destDir = if ($env:WIKI_BIN_DIR) { $env:WIKI_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\llm-wiki' }
$dest = Join-Path $destDir 'llm-wiki.exe'

$healthy = $false
if ((Test-Path -LiteralPath $dest) -and (-not $Force)) {
  try {
    & $dest version | Out-Null
    $healthy = $LASTEXITCODE -eq 0
  } catch {
    $healthy = $false
  }
}
if ($healthy) {
  Write-Host "llm-wiki is already installed at $dest (re-run with -Force to refresh)"
  exit 0
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null
Copy-Item -LiteralPath $src -Destination $dest -Force

try {
  & $dest version | Out-Null
  if ($LASTEXITCODE -ne 0) { throw 'non-zero exit' }
} catch {
  Write-Error "copied $dest but it does not run on this machine."
}

Write-Host "installed llm-wiki -> $dest"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -and ($userPath -split ';' -contains $destDir)) {
  exit 0
}
Write-Host "note: $destDir is not on your user PATH; add it with:"
Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$destDir;`" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')"
