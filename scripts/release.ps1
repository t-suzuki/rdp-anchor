# RDP Anchor release script
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/release.ps1                  # build only
#   powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version 0.1.0   # bump + commit + tag + build

param(
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"

# ── Version bump ──────────────────────────────────────────────────────
if ($Version) {
    Write-Host "=== Bumping version to $Version ===" -ForegroundColor Cyan

    # Validate semver-ish format
    if ($Version -notmatch '^\d+\.\d+\.\d+$') {
        Write-Error "Version must be in X.Y.Z format (e.g. 0.1.0)"
        exit 1
    }

    # Check for uncommitted changes
    $status = git status --porcelain
    if ($status) {
        Write-Error "Working directory is not clean. Commit or stash changes first."
        exit 1
    }

    # Files to update (pattern: old version literal → new version)
    # We find current version from Cargo.toml
    $cargoContent = Get-Content "Cargo.toml" -Raw
    if ($cargoContent -match 'version\s*=\s*"(\d+\.\d+\.\d+)"') {
        $oldVersion = $Matches[1]
    } else {
        Write-Error "Could not detect current version from Cargo.toml"
        exit 1
    }

    if ($oldVersion -eq $Version) {
        Write-Error "Version is already $Version"
        exit 1
    }

    Write-Host "  $oldVersion -> $Version"

    # Cargo.toml: version = "X.Y.Z"
    (Get-Content "Cargo.toml" -Encoding UTF8) -replace "version = `"$oldVersion`"", "version = `"$Version`"" |
        Set-Content "Cargo.toml" -Encoding UTF8

    # tauri.conf.json: "version": "X.Y.Z" and "title": "RDP Anchor vX.Y.Z"
    (Get-Content "tauri.conf.json" -Encoding UTF8) `
        -replace "`"version`": `"$oldVersion`"", "`"version`": `"$Version`"" `
        -replace "RDP Anchor v$oldVersion", "RDP Anchor v$Version" |
        Set-Content "tauri.conf.json" -Encoding UTF8

    # dist/index.html: <title> and <h1> version
    (Get-Content "dist/index.html" -Encoding UTF8) `
        -replace "v$oldVersion", "v$Version" |
        Set-Content "dist/index.html" -Encoding UTF8

    # scripts/package-zip.ps1: $version = "X.Y.Z"
    (Get-Content "scripts/package-zip.ps1" -Encoding UTF8) `
        -replace "\`$version = `"$oldVersion`"", "`$version = `"$Version`"" |
        Set-Content "scripts/package-zip.ps1" -Encoding UTF8

    # README.md: version in artifact paths
    (Get-Content "README.md" -Encoding UTF8) `
        -replace [regex]::Escape($oldVersion), $Version |
        Set-Content "README.md" -Encoding UTF8

    # Commit and tag
    git add Cargo.toml tauri.conf.json "dist/index.html" "scripts/package-zip.ps1" README.md
    git commit -m "Release v$Version"
    git tag "v$Version"

    Write-Host "  Committed and tagged v$Version" -ForegroundColor Green
}

# ── Build ─────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "=== Building ===" -ForegroundColor Cyan
cargo tauri build
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}

# ── Package ZIP ───────────────────────────────────────────────────────
Write-Host ""
Write-Host "=== Packaging ZIP ===" -ForegroundColor Cyan
& powershell -ExecutionPolicy Bypass -File scripts/package-zip.ps1

# ── Summary ───────────────────────────────────────────────────────────
Write-Host ""
Write-Host "=== Release artifacts ===" -ForegroundColor Green

$v = if ($Version) { $Version } else {
    $c = Get-Content "Cargo.toml" -Raw
    if ($c -match 'version\s*=\s*"(\d+\.\d+\.\d+)"') { $Matches[1] } else { "unknown" }
}

$exe = "target/release/rdp-anchor.exe"
$msiEn = "target/release/bundle/msi/RDP Anchor_${v}_x64_en-US.msi"
$msiJa = "target/release/bundle/msi/RDP Anchor_${v}_x64_ja-JP.msi"
$zip = "target/release/bundle/zip/RDP-Anchor_${v}_x64.zip"

foreach ($f in @($exe, $msiEn, $msiJa, $zip)) {
    if (Test-Path $f) {
        $size = [math]::Round((Get-Item $f).Length / 1MB, 1)
        Write-Host "  $f  (${size} MB)"
    } else {
        Write-Host "  $f  (not found)" -ForegroundColor Yellow
    }
}

if ($Version) {
    Write-Host ""
    Write-Host "Tagged v$Version. Push with: git push && git push --tags" -ForegroundColor Cyan
}
