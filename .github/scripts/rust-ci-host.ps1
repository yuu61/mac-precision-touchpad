#Requires -Version 7

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Packages
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$pkgArgs = @()
foreach ($pkg in ($Packages -split '\s+' | Where-Object { $_ })) {
    $pkgArgs += @('-p', $pkg)
}

if ($pkgArgs.Count -eq 0) {
    throw 'Packages must contain at least one Cargo package name.'
}

cargo test --locked @pkgArgs

foreach ($var in 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS') {
    Remove-Item "Env:$var" -ErrorAction SilentlyContinue
}

cargo clippy --locked --all-targets --all-features @pkgArgs -- -D warnings
