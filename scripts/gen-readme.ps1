# Generate per-language README files from docs/README.template.md
# Usage: powershell -ExecutionPolicy Bypass -File scripts/gen-readme.ps1
#
# Template format:
#   *: line       → included in all languages
#   en: line      → included only in en_US
#   ja: line      → included only in ja_JP
#   (blank line)  → included in all languages

param(
    [string]$Template = "docs/README.template.md",
    [string]$OutDir = "docs"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Template)) {
    Write-Error "Template not found: $Template"
    exit 1
}

$langs = @{
    "en" = "README.en_US.md"
    "ja" = "README.ja_JP.md"
}

$lines = Get-Content $Template -Encoding UTF8

foreach ($entry in $langs.GetEnumerator()) {
    $lang = $entry.Key
    $outFile = Join-Path $OutDir $entry.Value
    $output = [System.Collections.Generic.List[string]]::new()

    foreach ($line in $lines) {
        if ($line -match "^\*: ?(.*)$") {
            $output.Add($Matches[1])
        }
        elseif ($line -match "^${lang}: ?(.*)$") {
            $output.Add($Matches[1])
        }
        elseif ($line -eq "") {
            $output.Add("")
        }
        # else: line for another language, skip
    }

    # Trim trailing blank lines
    while ($output.Count -gt 0 -and $output[$output.Count - 1] -eq "") {
        $output.RemoveAt($output.Count - 1)
    }

    $output | Out-File -FilePath $outFile -Encoding UTF8
    Write-Host "Generated: $outFile"
}
