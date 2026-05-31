# dbopt installer for Windows (PowerShell).
#   irm https://dbopt.org/install.ps1 | iex
# Downloads the latest release, installs dbopt.exe to %LOCALAPPDATA%\dbopt and
# adds it to your user PATH.
$ErrorActionPreference = 'Stop'

$repo  = 'singhpratech/dbopt'
$asset = 'dbopt-windows-x86_64.zip'
$url   = "https://github.com/$repo/releases/latest/download/$asset"
$dest  = Join-Path $env:LOCALAPPDATA 'dbopt'

Write-Host "dbopt: downloading $asset ..."
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$zip = Join-Path $env:TEMP $asset
Invoke-WebRequest -Uri $url -OutFile $zip
Expand-Archive -Path $zip -DestinationPath $dest -Force
Remove-Item $zip -Force

# Add to the user PATH if missing
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
  Write-Host "dbopt: added $dest to your PATH (restart your terminal)."
}

# Start Menu shortcut so dbopt is launchable without a terminal (mirrors the MSI).
$exe = Join-Path $dest 'dbopt.exe'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\dbopt.lnk'
try {
  $ws = New-Object -ComObject WScript.Shell
  $sc = $ws.CreateShortcut($lnk)
  $sc.TargetPath = $exe
  $sc.WorkingDirectory = $dest
  $sc.WindowStyle = 7   # minimized — keep the server console out of the way
  $sc.Description = 'Open dbopt (opens http://127.0.0.1:3690)'
  $sc.Save()
  Write-Host "dbopt: added a Start Menu shortcut."
} catch {
  Write-Host "dbopt: (could not create Start Menu shortcut: $_)"
}

Write-Host "dbopt: installed to $exe"
Write-Host "dbopt: launch from the Start Menu (search 'dbopt'), or run 'dbopt' in a new terminal, then open http://127.0.0.1:3690"
