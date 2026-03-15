# RDP Launcher ZIP packaging script
# Usage: powershell -ExecutionPolicy Bypass -File scripts/package-zip.ps1
# Run after: cargo tauri build

$ErrorActionPreference = "Stop"

$version = "0.0.1"
$exe = "target/release/rdp-launcher.exe"
$zipName = "RDP-Launcher_${version}_x64.zip"
$outDir = "target/release/bundle/zip"
$stagingDir = "$outDir/_staging"

if (-not (Test-Path $exe)) {
    Write-Error "Build first: cargo tauri build"
    exit 1
}

# Generate per-language READMEs from template
Write-Host "Generating READMEs..."
& powershell -ExecutionPolicy Bypass -File scripts/gen-readme.ps1

# Clean and create staging directory
if (Test-Path $stagingDir) { Remove-Item $stagingDir -Recurse -Force }
New-Item -ItemType Directory -Path $stagingDir -Force | Out-Null

# Copy files
Copy-Item $exe $stagingDir/
Copy-Item "docs/README.en_US.md" $stagingDir/
Copy-Item "docs/README.ja_JP.md" $stagingDir/

# Create ZIP
$zipPath = "$outDir/$zipName"
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Compress-Archive -Path "$stagingDir/*" -DestinationPath $zipPath

# Clean up staging
Remove-Item $stagingDir -Recurse -Force

$fullPath = Resolve-Path $zipPath
Write-Host "Created: $fullPath"
