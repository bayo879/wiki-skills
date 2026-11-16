#requires -Version 5.1
<#
.SYNOPSIS
  Bootstrap llm-wiki on Windows: ensure Python 3.12, then install the skill.

.DESCRIPTION
  Finds an existing Python 3.12 (per-user install, py -3.12, python, or python3).
  If none is usable, downloads Python 3.12.8 from China mirrors (Huawei Cloud,
  TUNA, npmmirror), falls back to the official source and then winget, installs
  it per-user, refreshes PATH, and runs install.py.

.PARAMETER Target
  Passed to install.py --target. Defaults to auto.

.PARAMETER All
  Passed to install.py --all.

.PARAMETER DryRun
  Check and print the plan only. Does not download, install, or copy.

.PARAMETER Force
  Passed to install.py --force.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1 -Target "$env:USERPROFILE\.dsh\skills"
#>
[CmdletBinding()]
param(
    [string]$Target = 'auto',
    [switch]$All,
    [switch]$DryRun,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

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
            code = $Code
            message = $Message
            next = $Next
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

function Find-Python {
    $directCandidates = @()
    if ($env:LOCALAPPDATA) {
        $directCandidates += (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe')
    }
    foreach ($direct in $directCandidates) {
        if (-not (Test-Path $direct)) { continue }
        $version = Get-PythonVersion -Exe $direct -PrefixArgs @()
        if ($null -ne $version -and $version.Major -eq 3 -and $version.Minor -eq 12) {
            return [pscustomobject]@{
                Exe     = $direct
                Args    = @()
                Version = $version.Text
            }
        }
    }
    $candidates = @(
        [pscustomobject]@{ Name = 'py';      Args = @('-3.12') },
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
        $version = Get-PythonVersion -Exe $path -PrefixArgs $candidate.Args
        if ($null -eq $version) { continue }
        if ($version.Major -eq 3 -and $version.Minor -eq 12) {
            return [pscustomobject]@{
                Exe     = $path
                Args    = $candidate.Args
                Version = $version.Text
            }
        }
        if ($null -eq $old) { $old = $version.Text }
    }
    return [pscustomobject]@{ Exe = $null; Args = @(); Version = $old }
}

function Refresh-Path {
    $current = @($env:Path -split ';' | Where-Object { $_ })
    foreach ($scope in @('Machine', 'User')) {
        $value = [Environment]::GetEnvironmentVariable('Path', $scope)
        if (-not $value) { continue }
        foreach ($entry in @($value -split ';')) {
            if (-not $entry) { continue }
            if ($current -notcontains $entry) {
                $env:Path = $env:Path + ';' + $entry
                $current += $entry
            }
        }
    }
}

function Get-PythonUrls {
    param([string]$Arch)
    $version = '3.12.8'
    $file = "python-$version-$Arch.exe"
    if (-not $Arch) { $file = "python-$version.exe" }
    return @(
        "https://mirrors.huaweicloud.com/python/$version/$file",
        "https://mirrors.tuna.tsinghua.edu.cn/python/$version/$file",
        "https://registry.npmmirror.com/-/binary/python/$version/$file",
        "https://www.python.org/ftp/python/$version/$file"
    )
}

function Save-PythonInstaller {
    param([string]$Url, [string]$Path)
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Path -UseBasicParsing -TimeoutSec 300 -ErrorAction Stop
    } catch {
        $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
        if ($null -eq $curl) { return $false }
        $curlPath = $curl.Path
        if (-not $curlPath) { $curlPath = $curl.Source }
        & $curlPath -L --connect-timeout 15 --max-time 300 -o $Path $Url 2>$null
        if ($LASTEXITCODE -ne 0) { return $false }
    }
    if (-not (Test-Path $Path)) { return $false }
    try {
        $item = Get-Item $Path
        if ($item.Length -lt 1MB) { return $false }
        $stream = [IO.File]::OpenRead($Path)
        try {
            $buffer = New-Object byte[] 2
            $read = $stream.Read($buffer, 0, 2)
            if ($read -lt 2) { return $false }
            # Reject HTML error pages saved by a proxy or a login redirect.
            return ($buffer[0] -eq 0x4D -and $buffer[1] -eq 0x5A)
        } finally {
            $stream.Close()
        }
    } catch {
        return $false
    }
}

function Install-Python {
    param([string]$Arch, [string[]]$Urls)
    $dir = Join-Path $env:TEMP 'llm-wiki-bootstrap'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $fileName = Split-Path -Leaf $Urls[0]
    $installer = Join-Path $dir $fileName
    foreach ($url in $Urls) {
        Write-Step "trying $url"
        if (-not (Save-PythonInstaller -Url $url -Path $installer)) {
            Write-Step 'download failed or the file is not a Windows installer'
            continue
        }
        Write-Step 'installing Python 3.12.8 for the current user'
        Unblock-File -Path $installer -ErrorAction SilentlyContinue
        $arguments = @(
            '/quiet',
            'InstallAllUsers=0',
            'PrependPath=1',
            'Include_test=0',
            'Include_doc=0',
            'Include_dev=0',
            'Include_tcltk=0',
            'Include_launcher=1'
        )
        try {
            $process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru
        } catch {
            Write-Step "could not start the installer: $($_.Exception.Message)"
            continue
        }
        if ($process.ExitCode -eq 0 -or $process.ExitCode -eq 3010) {
            Refresh-Path
            $found = Find-Python
            if ($found.Exe) { return $found }
            $direct = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
            if (Test-Path $direct) {
                $version = Get-PythonVersion -Exe $direct -PrefixArgs @()
                if ($null -ne $version -and $version.Major -eq 3 -and $version.Minor -eq 12) {
                    return [pscustomobject]@{
                        Exe     = $direct
                        Args    = @()
                        Version = $version.Text
                    }
                }
            }
        } else {
            Write-Step "installer exited with code $($process.ExitCode)"
        }
    }
    return $null
}

function Install-PythonWithWinget {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($null -eq $winget) { return $null }
    $wingetPath = $winget.Path
    if (-not $wingetPath) { $wingetPath = $winget.Source }
    Write-Step 'falling back to winget'
    & $wingetPath install --id Python.Python.3.12 -e --silent --accept-package-agreements --accept-source-agreements | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Step "winget exited with code $LASTEXITCODE"
        return $null
    }
    Refresh-Path
    return Find-Python
}

try {
$arch = 'amd64'
if ($env:PROCESSOR_ARCHITEW6432 -eq 'AMD64') { $arch = 'amd64' }
elseif ($env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') { $arch = 'arm64' }
elseif ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { $arch = 'arm64' }
elseif ($env:PROCESSOR_ARCHITECTURE -eq 'x86') { $arch = '' }
$urls = Get-PythonUrls -Arch $arch

$python = Find-Python
if (-not $python.Exe) {
    if ($python.Version) {
        Write-Step "found Python $($python.Version), but this bootstrap requires 3.12"
    }
    if ($DryRun) {
        $plan = [pscustomobject]@{
            ok     = $true
            action = 'would-install-python'
            version = '3.12.8'
            arch   = $arch
            urls   = $urls
        } | ConvertTo-Json -Depth 4 -Compress
        Write-Output $plan
        exit 0
    }
    Write-Step 'Python 3.12 not found; trying China mirrors'
    $python = Install-Python -Arch $arch -Urls $urls
    if (-not $python.Exe) {
        $python = Install-PythonWithWinget
    }
}
if (-not $python.Exe) {
    Write-Failure -Code 'E_PYTHON' -Message 'Python 3.12 is required but was not found or installed.' -Next 'Install Python 3.12 manually from https://www.python.org/downloads/windows/ (select Add python.exe to PATH), then rerun bootstrap.ps1.'
}
Write-Step "using Python $($python.Version) at $($python.Exe)"

$installer = Join-Path $PSScriptRoot 'install.py'
if (-not (Test-Path $installer)) {
    Write-Failure -Code 'E_INSTALLER' -Message 'install.py was not found next to bootstrap.ps1.' -Next 'Run bootstrap.ps1 from the llm-wiki scripts directory.'
}

# Keep JSON and non-ASCII paths stable when the agent captures stdout.
$env:PYTHONUTF8 = '1'
$env:PYTHONIOENCODING = 'utf-8'

$installerArgs = @($installer, '--json')
if ($Target -ne 'auto') { $installerArgs += @('--target', $Target) }
if ($All) { $installerArgs += '--all' }
if ($DryRun) { $installerArgs += '--dry-run' }
if ($Force) { $installerArgs += '--force' }

$pythonArgs = @()
if ($python.Args) { $pythonArgs = @($python.Args) }
$allArgs = $pythonArgs + $installerArgs
& $python.Exe @allArgs
exit $LASTEXITCODE
} catch {
    Write-Failure -Code 'E_BOOTSTRAP' -Message $_.Exception.Message -Next 'Rerun bootstrap.ps1 -DryRun to inspect the plan. If it still fails, install Python 3.12 manually and run install.py.'
}
