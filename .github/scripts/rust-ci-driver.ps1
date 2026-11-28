#Requires -Version 7

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Target,

    [Parameter(Mandatory)]
    [string]$Packages,

    [string]$Toolchain = $env:RUSTUP_TOOLCHAIN,

    [switch]$LintSupport
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

$sanitizedVars = @(
    'CARGO_TARGET_DIR',
    'CARGO_BUILD_TARGET_DIR',
    'CARGO_BUILD_TARGET',
    'RUSTFLAGS',
    'CARGO_ENCODED_RUSTFLAGS',
    'CARGO_BUILD_RUSTFLAGS'
)
foreach ($var in $sanitizedVars) {
    Remove-Item "Env:$var" -ErrorAction SilentlyContinue
}

Push-Location 'drivers'
try {
    if ($pkgArgs -contains 'amtptp-device-spi-km') {
        $spiDependencies = cargo tree --locked --target $Target -p amtptp-device-spi-km --edges normal,build --prefix none --color never
        if ($spiDependencies -match '^amtptp-usb-bt v') {
            throw "The MIT SPI driver must not depend on the GPLv2 amtptp-usb-bt crate ($Target)."
        }
    }

    if ($LintSupport) {
        if ([string]::IsNullOrWhiteSpace($Toolchain)) {
            $rustc = (rustup which rustc).Trim()
        }
        else {
            $rustc = (rustup which --toolchain $Toolchain rustc).Trim()
        }
        $clippyDriver = Join-Path (Split-Path $rustc -Parent) 'clippy-driver.exe'
        if (-not (Test-Path $clippyDriver -PathType Leaf)) {
            throw "clippy-driver.exe not found at $clippyDriver"
        }

        Remove-Item Env:RUSTC_WORKSPACE_WRAPPER -ErrorAction SilentlyContinue
        $previousWrapper = $env:RUSTC_WRAPPER
        $env:RUSTC_WRAPPER = $clippyDriver
        try {
            cargo check --locked --all-features --target $Target @pkgArgs --config 'build.rustflags=["-Dwarnings"]'
        }
        finally {
            if ($null -eq $previousWrapper) {
                Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue
            }
            else {
                $env:RUSTC_WRAPPER = $previousWrapper
            }
        }
    }
    else {
        cargo clippy --locked --all-features --target $Target @pkgArgs -- -D warnings
    }
}
finally {
    Pop-Location
}
