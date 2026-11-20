<#
.SYNOPSIS
    Copies the fork's komorebi binaries to a stable folder and starts them at login.

.DESCRIPTION
    1. Copies komorebi.exe and komorebi-bar.exe from the fork's build folder to ForkDir,
       so rebuilding the fork never breaks the running setup. Run again after rebuilding.
    2. Copies start-komorebi.ps1 next to them.
    3. Creates "komorebi (komotype).lnk" in the startup folder.
    Remove the shortcut to undo the autostart.

.PARAMETER ForkRepo
    Path of the cloned fork (https://github.com/northkey-dev/komorebi, branch komotype),
    built with: cargo build --release -p komorebi -p komorebi-bar
#>
param(
    [Parameter(Mandatory = $true)][string]$ForkRepo,
    [string]$ForkDir = "$env:LOCALAPPDATA\komotype\komorebi-fork",
    [string]$BarConfig = "$env:USERPROFILE\komorebi.bar.json"
)

$ErrorActionPreference = 'Stop'

$release = Join-Path $ForkRepo 'target\release'
New-Item -ItemType Directory -Force $ForkDir | Out-Null
foreach ($exe in 'komorebi.exe', 'komorebi-bar.exe') {
    $source = Join-Path $release $exe
    if (-not (Test-Path $source)) { throw "$source not found. Build the fork first." }
    Copy-Item $source $ForkDir -Force
}
$script = Join-Path $ForkDir 'start-komorebi.ps1'
Copy-Item (Join-Path $PSScriptRoot 'start-komorebi.ps1') $script -Force

$startup = [Environment]::GetFolderPath('Startup')
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut((Join-Path $startup 'komorebi (komotype).lnk'))
$link.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
$link.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$script`" -ForkDir `"$ForkDir`" -BarConfig `"$BarConfig`""
$link.WindowStyle = 7
$link.Description = 'Start komorebi, whkd and komorebi-bar (komotype fork)'
$link.Save()

Write-Host "Installed to $ForkDir"
Write-Host "Autostart: $(Join-Path $startup 'komorebi (komotype).lnk')"
