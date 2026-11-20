<#
.SYNOPSIS
    Starts komorebi, whkd and komorebi-bar from the komotype fork.

.DESCRIPTION
    Meant for the Windows startup folder (see install-autostart.ps1). Starts the fork's
    komorebi.exe (needed for "remember_layouts"), whkd if installed, waits until komorebi
    answers, then starts the fork's komorebi-bar with the given config. The bar starts
    komotyped by itself. Programs that are already running are left alone.

.PARAMETER ForkDir
    Folder with the fork's komorebi.exe and komorebi-bar.exe.

.PARAMETER BarConfig
    komorebi.bar.json that contains the Komotype widget.
#>
param(
    [string]$ForkDir = "$env:LOCALAPPDATA\komotype\komorebi-fork",
    [string]$BarConfig = "$env:USERPROFILE\komorebi.bar.json"
)

$ErrorActionPreference = 'Stop'

function Test-Running([string]$Name) {
    return [bool](Get-Process -Name $Name -ErrorAction SilentlyContinue)
}

if (-not (Test-Running 'komorebi')) {
    Start-Process -FilePath (Join-Path $ForkDir 'komorebi.exe') -WindowStyle Hidden
}

$whkd = Get-Command whkd.exe -ErrorAction SilentlyContinue
if ($whkd -and -not (Test-Running 'whkd')) {
    Start-Process -FilePath $whkd.Source -WindowStyle Hidden
}

# The bar reads monitor information from komorebi on start, so wait until it answers.
$komorebic = Get-Command komorebic.exe -ErrorAction SilentlyContinue
if ($komorebic) {
    for ($i = 0; $i -lt 30; $i++) {
        # Windows PowerShell turns native stderr into errors; komorebic writes there until
        # komorebi is ready, so check the exit code of a separate process instead.
        $probe = Start-Process -FilePath $komorebic.Source -ArgumentList 'state' -WindowStyle Hidden `
            -Wait -PassThru -RedirectStandardOutput "$env:TEMP\komotype-probe.out" `
            -RedirectStandardError "$env:TEMP\komotype-probe.err"
        if ($probe.ExitCode -eq 0) { break }
        Start-Sleep -Milliseconds 500
    }
} else {
    Start-Sleep -Seconds 3
}

if (-not (Test-Running 'komorebi-bar')) {
    Start-Process -FilePath (Join-Path $ForkDir 'komorebi-bar.exe') `
        -ArgumentList '--config', "`"$BarConfig`"" -WindowStyle Hidden
}
