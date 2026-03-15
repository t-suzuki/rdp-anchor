# RDP Anchor task runner
# Install: cargo install just
# Usage:  just dev | just build | just release 0.1.0

# Development mode
dev:
    cargo tauri dev

# Build only (no version bump)
build:
    cargo tauri build
    powershell -ExecutionPolicy Bypass -File scripts/package-zip.ps1

# Release with version bump: just release 0.1.0
release version:
    powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Version {{version}}

# Build without version bump
release-build:
    powershell -ExecutionPolicy Bypass -File scripts/release.ps1

# Generate bilingual READMEs from template
gen-readme:
    powershell -ExecutionPolicy Bypass -File scripts/gen-readme.ps1

# Check compilation
check:
    cargo check

# Generate icons from source PNG
icon path:
    cargo tauri icon {{path}}
