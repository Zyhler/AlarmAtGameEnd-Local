$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$Dist = Join-Path $Root "dist"
$PackageDir = Join-Path $Dist "Alarm at Game End"
$SourceExe = Join-Path $Root "target\release\alarm_at_game_end.exe"
$PackageExe = Join-Path $PackageDir "Alarm at Game End.exe"
$ZipPath = Join-Path $Dist "Alarm at Game End-windows-x64.zip"

cargo build --release --manifest-path (Join-Path $Root "Cargo.toml")

New-Item -ItemType Directory -Force -Path $PackageDir | Out-Null

if (Test-Path $PackageExe) {
    Remove-Item -LiteralPath $PackageExe -Force
}

Copy-Item -LiteralPath $SourceExe -Destination $PackageExe

if (Test-Path $ZipPath) {
    Remove-Item -LiteralPath $ZipPath -Force
}

Compress-Archive -Path $PackageExe -DestinationPath $ZipPath

Write-Host "Packaged $ZipPath"
