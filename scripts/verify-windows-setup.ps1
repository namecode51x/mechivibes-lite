# Verifies the Windows setup end to end, locally.
#
# The release pipeline builds this installer on CI and the in-app updater
# installs it silently. This script exercises the same paths on a developer
# machine, without publishing anything:
#
#   1. The installer asset exists and its name matches the contract the
#      in-app updater filters on ("x64" + ".exe") - the same check the
#      release pipeline asserts.
#   2. The asset's SHA256 is written to a SHA256SUMS.txt in the format the
#      app's parser accepts, and the digest verifies against the file.
#   3. A silent install (the exact arguments the updater spawns it with)
#      lays down the app, its assets and its soundpacks.
#   4. The installed app runs and resolves its assets from beside the
#      executable, not from the source tree.
#   5. A second silent install over the same directory is an in-place
#      upgrade, which is what an auto-update does.
#
# Everything lands in a temp directory that is removed afterwards, and the
# registry/uninstall entries the test install creates are removed too.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\verify-windows-setup.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\verify-windows-setup.ps1 -KeepArtifacts

param(
    # Leave the temp install directory behind for inspection.
    [switch]$KeepArtifacts = $false
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
Set-Location $ProjectRoot

$work = Join-Path $env:TEMP "mv-setup-verify"
$installDir = Join-Path $work "installed"
$logFile = Join-Path $work "install.log"
# Set when the "installed app runs" check is skipped because another
# instance already holds the single-instance mutex.
$appRunCheckSkipped = $false

function Write-Step([string]$text) {
    Write-Host ""
    Write-Host "=== $text ===" -ForegroundColor Cyan
}

function Fail([string]$text) {
    Write-Host "ERROR: $text" -ForegroundColor Red
    exit 1
}

# A silent install relaunches the app on purpose - that [Run] entry marked
# `skipifnotsilent` is the ONE path that brings the app back after an
# auto-update (see the .iss). It takes the single-instance mutex, so any
# attempt to run the installed app afterwards would just print "already
# running". Kill the relaunched copy first, and only the one that lives
# under $work, never the developer's own instance.
function Stop-RelaunchedApp() {
    Get-Process mechvibes-dx -ErrorAction SilentlyContinue | ForEach-Object {
        try {
            if ($_.Path -and $_.Path.StartsWith($work, [System.StringComparison]::OrdinalIgnoreCase)) {
                Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
            }
        } catch { }
    }
}

# Runs the installer silently and waits only for SETUP to finish.
#
# `Start-Process -Wait` waits for the whole process tree, and the .iss's
# [Run] entry spawns the app with `nowait` - so -Wait would block until the
# relaunched app is closed, which looks exactly like a hung install. Poll the
# setup process itself instead, then stop whatever it relaunched.
function Invoke-SilentInstall([string]$logPath, [int]$timeoutSec = 240) {
    # NB: `$args` is a PowerShell automatic variable - never assign to it.
    $innoArgs = $silentArgs + @("/DIR=`"$installDir`"", "/LOG=`"$logPath`"")
    $p = Start-Process -FilePath $installer.FullName -ArgumentList $innoArgs -PassThru
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while (-not $p.HasExited -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
    }
    if (-not $p.HasExited) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Fail "installer did not finish within ${timeoutSec}s - see $logPath"
    }
    Stop-RelaunchedApp
    return $p.ExitCode
}

# --- 0. Preconditions -------------------------------------------------------
Write-Step "Preconditions"

$dist = Join-Path $ProjectRoot "dist"
if (-not (Test-Path $dist)) { Fail "no dist/ directory - run scripts\build-windows-installer.ps1 first" }

$installer = Get-ChildItem $dist -Filter "MechvibesDX-*-Setup-x64.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { Fail "no MechvibesDX-*-Setup-x64.exe in dist/" }

# The in-app updater picks the first release asset whose name contains "x64"
# and ends in ".exe" (src/utils/auto_updater.rs, asset_matches). Assert the
# same contract here rather than trusting it.
if ($installer.Name -notmatch "x64" -or $installer.Name -notmatch "\.exe$") {
    Fail "installer name '$($installer.Name)' does not match the updater's filter (needs 'x64' and '.exe')"
}
Write-Host "  installer: $($installer.Name) ($([math]::Round($installer.Length/1MB,1)) MB)" -ForegroundColor Green

# The updater spawns the installer with these exact arguments
# (src/utils/update_installer.rs, INSTALLER_SILENT_ARGS).
$silentArgs = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART")

if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Path $work | Out-Null
New-Item -ItemType Directory -Path $installDir | Out-Null

# --- 1. Checksum contract ----------------------------------------------------
Write-Step "Checksum contract (the release pipeline publishes SHA256SUMS.txt)"

$sums = Join-Path $work "SHA256SUMS.txt"
# The updater downloads the release asset into its staging directory and
# verifies it there, so the digest is computed over the downloaded copy -
# which is the installer itself.
$digest = (Get-FileHash $installer.FullName -Algorithm SHA256).Hash.ToLower()
"$digest  $($installer.Name)" | Out-File -FilePath $sums -Encoding utf8 -Append
Get-Content $sums | ForEach-Object { "  $_" }

# The app's parser (parse_sha256sums in src/utils/update_installer.rs) accepts
# "<64 hex><whitespace>[*]<filename>" and matches on the basename.
$line = Get-Content $sums | Select-Object -First 1
if ($line -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
    Fail "the app's checksum parser would reject this line: $line"
}
$expected = $Matches[1].ToLower()
$named = $Matches[2]
$actual = (Get-FileHash $installer.FullName -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected) { Fail "digest mismatch: expected $expected, got $actual" }
if ($named -ne $installer.Name) { Fail "the sums file names '$named' but the asset is '$($installer.Name)'" }
Write-Host "  digest verifies against the file: OK" -ForegroundColor Green

# --- 2. Silent install -------------------------------------------------------
Write-Step "Silent install (the updater's exact arguments)"

$exitCode = Invoke-SilentInstall $logFile
Write-Host "  installer exit code: $exitCode"
if ($exitCode -ne 0) { Fail "installer exited $exitCode - see $logFile" }

$appExe = Join-Path $installDir "mechvibes-dx.exe"
if (-not (Test-Path $appExe)) { Fail "the app was not installed" }
$fileCount = (Get-ChildItem $installDir -Recurse -File).Count
Write-Host "  app installed: $fileCount files" -ForegroundColor Green

# Assets must sit beside the executable: the Dioxus asset resolver looks for
# `assets/` next to the exe (see the .iss and src/state/paths.rs).
if (-not (Test-Path (Join-Path $installDir "assets\sounds"))) {
    Fail "the installed app has no assets\sounds beside the executable"
}
Write-Host "  assets resolve from beside the executable: OK" -ForegroundColor Green

# User state is NOT shipped: the app creates data/ itself on first run.
if (Test-Path (Join-Path $installDir "data")) {
    Fail "the install contains a data/ directory - that is user state and must not be shipped"
}
Write-Host "  no user state shipped: OK" -ForegroundColor Green

# --- 3. The installed app runs ----------------------------------------------
Write-Step "The installed app runs"

$appOut = Join-Path $work "app.log"
$appErr = Join-Path $work "app.err"
$app = Start-Process -FilePath $appExe -ArgumentList "--headless" -WorkingDirectory $installDir `
    -RedirectStandardOutput $appOut -RedirectStandardError $appErr -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 10

if ($app.HasExited) {
    $out = Get-Content $appOut -Raw -ErrorAction SilentlyContinue
    $err = Get-Content $appErr -Raw -ErrorAction SilentlyContinue
    # The app takes a named single-instance mutex (src/libs/single_instance.rs).
    # If a copy is already running - the developer's own instance, most likely -
    # the installed one exits on purpose. That is correct behaviour, not a
    # broken install, so report it as a skip rather than a failure.
    if (($out + $err) -match "already running") {
        Write-Host "  skipped: another instance already holds the mutex" -ForegroundColor Yellow
        Write-Host "           (correct single-instance behaviour, not a broken install)" -ForegroundColor Yellow
        # Soft skip only - do NOT `return` here, or steps 4 and 5 would be
        # skipped too and the registry entry + temp dir would be left behind.
        $script:appRunCheckSkipped = $true
    }
    else {
        Write-Host "  the app exited early. Output:" -ForegroundColor Red
        Get-Content $appOut -ErrorAction SilentlyContinue | Select-Object -First 20 | ForEach-Object { "    $_" }
        Get-Content $appErr -ErrorAction SilentlyContinue | Select-Object -First 10 | ForEach-Object { "    $_" }
        Fail "the installed app did not stay running"
    }
}

if (-not $appRunCheckSkipped) {
    $log = Get-Content $appOut -Raw -ErrorAction SilentlyContinue
    if ($log -notmatch [regex]::Escape($installDir)) {
        Fail "the installed app did not resolve its app root from beside the executable"
    }
    Write-Host "  running, app root resolved from the install directory: OK" -ForegroundColor Green
    Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
}

# --- 4. In-place upgrade -----------------------------------------------------
Write-Step "In-place upgrade (a second silent install over the same directory)"

$exitCode2 = Invoke-SilentInstall $logFile
Write-Host "  second install exit code: $exitCode2"
if ($exitCode2 -ne 0) { Fail "the in-place upgrade failed - see $logFile" }
if (-not (Test-Path $appExe)) { Fail "the app disappeared after the upgrade" }
Write-Host "  upgrade over an existing install: OK" -ForegroundColor Green

# --- 5. Cleanup --------------------------------------------------------------
Write-Step "Cleanup"

# The test install registered itself for uninstall; remove that entry and the
# directory so the machine is left as it was found.
#
# NB: `Name` on a registry key is the FULL path (HKEY_CURRENT_USER\...\Uninstall\
# {GUID}_is1), so the wildcard must lead with `*` - a pattern anchored on "{"
# matches nothing and reports a silent "removed 0".
$uninstallBase = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall"
$stale = Get-ChildItem $uninstallBase -ErrorAction SilentlyContinue |
    Where-Object { $_.PSChildName -like "{8B5F5E5E*" }
foreach ($k in $stale) {
    Remove-Item -LiteralPath "$uninstallBase\$($k.PSChildName)" -Recurse -Force -ErrorAction SilentlyContinue
}
$stillThere = @(Get-ChildItem $uninstallBase -ErrorAction SilentlyContinue |
    Where-Object { $_.PSChildName -like "{8B5F5E5E*" }).Count
Write-Host "  removed $($stale.Count) uninstall entry/entries ($stillThere left)" -ForegroundColor Green
if ($stillThere -gt 0) { Fail "a test uninstall entry is still registered - the machine was not left clean" }

if ($KeepArtifacts) {
    Write-Host "  artifacts kept in: $work" -ForegroundColor Yellow
}
else {
    # The in-place upgrade relaunches the app from $installDir, and Windows
    # briefly keeps the executable's image handle after Stop-Process returns.
    # A single Remove-Item then leaves the .exe behind (with its directory)
    # while still printing "removed" - so stop anything still running from
    # there and retry until the tree is actually gone.
    Stop-RelaunchedApp
    for ($i = 1; $i -le 10 -and (Test-Path $work); $i++) {
        Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path $work) { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path $work) { Fail "could not remove $work - a process is still holding it open" }
    Write-Host "  removed $work" -ForegroundColor Green
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Setup verified end to end" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Cyan
