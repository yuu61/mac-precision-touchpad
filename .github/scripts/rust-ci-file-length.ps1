#Requires -Version 7

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$maxLines = 600
$repoRoot = (Resolve-Path -LiteralPath "$PSScriptRoot/../..").Path
$files = @(git -C $repoRoot -c core.quotepath=false ls-files --cached --others --exclude-standard -- '*.rs')
$violations = 0

foreach ($file in $files) {
    $path = Join-Path $repoRoot $file
    # Git still lists files deleted from the working tree until the deletion is staged.
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        continue
    }

    $lines = [System.IO.File]::ReadAllLines($path).Length
    if ($lines -gt $maxLines) {
        Write-Warning "${file}: $lines lines (maximum $maxLines)"
        $violations++
    }
}

if ($violations -gt 0) {
    throw "$violations Rust source file(s) exceed the $maxLines-line limit."
}
