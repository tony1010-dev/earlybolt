# Earlybolt installer for Windows
#   irm https://raw.githubusercontent.com/tony1010-dev/earlybolt/main/install.ps1 | iex
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = "tony1010-dev/earlybolt"
Write-Host "Installing Earlybolt..." -ForegroundColor Yellow

try {
    $release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ "User-Agent" = "earlybolt-installer" }
} catch { $release = $null }
$asset = $release.assets | Where-Object { $_.name -like "*-setup.exe" } | Select-Object -First 1
if (-not $asset) {
    Write-Host "No Windows installer is published yet (the release may still be building)." -ForegroundColor Red
    Write-Host "Check https://github.com/$repo/releases and try again in a few minutes."
    return
}

$setup = Join-Path $env:TEMP $asset.name
Invoke-WebRequest $asset.browser_download_url -OutFile $setup -UseBasicParsing

Get-Process earlybolt -ErrorAction SilentlyContinue | Stop-Process -Force
# Silent per-user install (no admin rights needed)
Start-Process $setup -ArgumentList "/S" -Wait
Remove-Item $setup -ErrorAction SilentlyContinue

$exe = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Earlybolt") -Filter "earlybolt.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
if ($exe) { Start-Process $exe.FullName }

Write-Host "Installed. Look for the bolt icon in the system tray (click ^ if it is hidden)." -ForegroundColor Green
