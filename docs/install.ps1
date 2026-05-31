# dbopt installer for Windows (PowerShell).
#   irm https://dbopt.org/install.ps1 | iex
# Downloads the latest release, installs dbopt.exe to %LOCALAPPDATA%\dbopt and
# adds it to your user PATH.
$ErrorActionPreference = 'Stop'

$repo  = 'singhpratech/dbopt'
$asset = 'sqlopt-windows-x86_64.zip'
$url   = "https://github.com/$repo/releases/latest/download/$asset"
$dest  = Join-Path $env:LOCALAPPDATA 'dbopt'

Write-Host "dbopt: downloading $asset ..."
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$zip = Join-Path $env:TEMP $asset
Invoke-WebRequest -Uri $url -OutFile $zip
Expand-Archive -Path $zip -DestinationPath $dest -Force
Remove-Item $zip -Force

# Install as dbopt.exe
if (Test-Path (Join-Path $dest 'sqlopt.exe')) {
  Move-Item -Force (Join-Path $dest 'sqlopt.exe') (Join-Path $dest 'dbopt.exe')
}

# Add to the user PATH if missing
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
  Write-Host "dbopt: added $dest to your PATH (restart your terminal)."
}

Write-Host "dbopt: installed to $dest\dbopt.exe"
Write-Host "dbopt: run 'dbopt' and open http://127.0.0.1:3690"
