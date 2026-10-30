$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$release = Join-Path $root 'target/release'
$dist = Join-Path $root 'dist/LocalPrivacyScreen'
$zip = Join-Path $root 'dist/LocalPrivacyScreen.zip'

cargo test --locked
if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }

cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }

New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item -LiteralPath (Join-Path $release 'local-privacy-screen.exe') -Destination (Join-Path $dist 'LocalPrivacyScreen.exe') -Force
Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $dist -Force
Compress-Archive -LiteralPath @(
    (Join-Path $dist 'LocalPrivacyScreen.exe'),
    (Join-Path $dist 'README.md')
) -DestinationPath $zip -Force

Get-Item -LiteralPath (Join-Path $dist 'LocalPrivacyScreen.exe'), $zip |
    Select-Object FullName, Length, LastWriteTime
Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dist 'LocalPrivacyScreen.exe'), $zip
Write-Output "Ready: $zip"
