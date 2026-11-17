#requires -Version 5.1
<#
.SYNOPSIS
  Find a Python 3.12+ runtime on Windows and install the llm-wiki skill.

.DESCRIPTION
  Looks for an existing Python 3.12 or newer: the path given with -Python, a
  per-user install under %LOCALAPPDATA%\Programs\Python, the py launcher, or
  python/python3 on PATH (WindowsApps redirect stubs are skipped).

  It never downloads or installs Python. When no suitable runtime is found it
  writes one JSON error object to stdout and exits 1, so the caller can hand
  the interpreter path over with -Python and rerun.

.PARAMETER Target
  Passed to install.py --target. Defaults to auto.

.PARAMETER All
  Passed to install.py --all.

.PARAMETER DryRun
  Passed to install.py --dry-run: print the plan, copy nothing.

.PARAMETER Force
  Passed to install.py --force.

.PARAMETER Python
  Path to a python.exe to use. Skips the search; the version is still checked.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1 -Python C:\runtime\python.exe -Target "$env:USERPROFILE\.dsh\skills"
#>
[CmdletBinding()]
param(
    [string]$Target = 'auto',
    [switch]$All,
    [switch]$DryRun,
    [switch]$Force,
    [string]$Python = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$MINIMUM = @(3, 12)
$MINIMUM_TEXT = '3.12'
$MANUAL = Join-Path (Split-Path $PSScriptRoot -Parent) 'references\recovery.md'

function Write-Step {
    param([string]$Message)
    # Keep stdout clean for the JSON emitted by install.py.
    [Console]::Error.WriteLine("[llm-wiki] $Message")
}

function Write-Failure {
    param([string]$Code, [string]$Message, [string]$Next)
    $payload = [pscustomobject]@{
        ok = $false
        error = [pscustomobject]@{
            code    = $Code
            message = $Message
            next    = $Next
            doc     = $MANUAL
            section = '运行时与安装'
        }
    } | ConvertTo-Json -Depth 4 -Compress
    Write-Output $payload
    exit 1
}

function Get-PythonVersion {
    param([string]$Exe, [string[]]$PrefixArgs)
    try {
        $output = & $Exe @PrefixArgs -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
        $line = @($output)[0]
        if ($null -eq $line) { return $null }
        $text = ([string]$line).Trim()
        if ($text -match '^(\d+)\.(\d+)$') {
            return [pscustomobject]@{
                Major = [int]$Matches[1]
                Minor = [int]$Matches[2]
                Text  = $text
            }
        }
    } catch {
        return $null
    }
    return $null
}

function Test-NewEnough {
    param($Version)
    if ($null -eq $Version) { return $false }
    return ($Version.Major -gt $MINIMUM[0]) -or
        ($Version.Major -eq $MINIMUM[0] -and $Version.Minor -ge $MINIMUM[1])
}

function Test-Candidate {
    param([string]$Exe, [string[]]$PrefixArgs = @())
    if (-not $Exe) { return $null }
    if (-not (Test-Path -LiteralPath $Exe)) { return $null }
    $version = Get-PythonVersion -Exe $Exe -PrefixArgs $PrefixArgs
    if (Test-NewEnough $version) {
        return [pscustomobject]@{
            Exe     = $Exe
            Args    = @($PrefixArgs)
            Version = $version.Text
        }
    }
    return [pscustomobject]@{ Exe = $null; Args = @(); Version = $(if ($version) { $version.Text } else { $null }) }
}

function Find-Python {
    $directs = @()
    if ($env:LOCALAPPDATA) {
        $root = Join-Path $env:LOCALAPPDATA 'Programs\Python'
        if (Test-Path -LiteralPath $root) {
            $directs += @(
                Get-ChildItem -LiteralPath $root -Directory -Filter 'Python3*' -ErrorAction SilentlyContinue |
                    Sort-Object Name -Descending |
                    ForEach-Object { Join-Path $_.FullName 'python.exe' }
            )
        }
    }
    foreach ($direct in $directs) {
        $found = Test-Candidate -Exe $direct
        if ($found.Exe) { return $found }
    }
    $candidates = @(
        [pscustomobject]@{ Name = 'py';      Args = @('-3') },
        [pscustomobject]@{ Name = 'python';  Args = @() },
        [pscustomobject]@{ Name = 'python3'; Args = @() }
    )
    $old = $null
    foreach ($candidate in $candidates) {
        $command = Get-Command $candidate.Name -ErrorAction SilentlyContinue
        if ($null -eq $command) { continue }
        $path = $command.Path
        if (-not $path) { $path = $command.Source }
        if (-not $path) { continue }
        # Redirection stubs in WindowsApps are not usable runtimes.
        if ($path -like '*\WindowsApps\*') { continue }
        $found = Test-Candidate -Exe $path -PrefixArgs $candidate.Args
        if ($found.Exe) { return $found }
        if ($found.Version -and -not $old) { $old = $found.Version }
    }
    return [pscustomobject]@{ Exe = $null; Args = @(); Version = $old }
}

try {
    if ($Python) {
        $python = Test-Candidate -Exe $Python
        if (-not $python.Exe) {
            $seen = if ($python.Version) { " found version $($python.Version);" } else { '' }
            Write-Failure -Code 'E_PYTHON' `
                -Message "the interpreter given with -Python is not usable:$seen need $MINIMUM_TEXT+ at $Python" `
                -Next "pass the python.exe of the runtime your agent already ships with -Python, or install Python $MINIMUM_TEXT+ and rerun"
        }
    } else {
        $python = Find-Python
        if (-not $python.Exe) {
            $seen = if ($python.Version) { " newest found is $($python.Version)," } else { '' }
            Write-Failure -Code 'E_PYTHON' `
                -Message "no Python $MINIMUM_TEXT+ runtime was found;$seen nothing was downloaded because this script does not install Python" `
                -Next "pass -Python <path-to-python.exe> (for example the interpreter your agent ships), or install Python $MINIMUM_TEXT+ and rerun"
        }
    }
    Write-Step "using Python $($python.Version) at $($python.Exe)"

    $installer = Join-Path $PSScriptRoot 'install.py'
    if (-not (Test-Path -LiteralPath $installer)) {
        Write-Failure -Code 'E_INSTALLER' -Message 'install.py was not found next to bootstrap.ps1.' -Next 'run bootstrap.ps1 from the llm-wiki scripts directory'
    }

    # Keep JSON and non-ASCII paths stable when the agent captures stdout.
    $env:PYTHONUTF8 = '1'
    $env:PYTHONIOENCODING = 'utf-8'

    $installerArgs = @($installer, '--json')
    if ($Target -ne 'auto') { $installerArgs += @('--target', $Target) }
    if ($All) { $installerArgs += '--all' }
    if ($DryRun) { $installerArgs += '--dry-run' }
    if ($Force) { $installerArgs += '--force' }

    $allArgs = @($python.Args) + $installerArgs
    & $python.Exe @allArgs
    exit $LASTEXITCODE
} catch {
    Write-Failure -Code 'E_BOOTSTRAP' -Message $_.Exception.Message -Next 'rerun with -DryRun to inspect the plan; if it still fails, run install.py with the same interpreter yourself'
}
