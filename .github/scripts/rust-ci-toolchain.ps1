#Requires -Version 7

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Toolchain,

    [ValidateSet('x64', 'arm64')]
    [string]$Arch = 'x64',

    [string]$Target = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$rustupArgs = @(
    'toolchain', 'install', $Toolchain,
    '--profile', 'minimal',
    '--component', 'clippy'
)

if ($Arch -eq 'arm64' -and -not [string]::IsNullOrWhiteSpace($Target)) {
    $rustupArgs += @('--target', $Target)
}

rustup @rustupArgs
