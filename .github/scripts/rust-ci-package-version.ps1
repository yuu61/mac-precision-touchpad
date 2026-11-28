#Requires -Version 7

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('x64', 'arm64')]
    [string]$Arch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$package = "target/package/$Arch/AmtPtpDevice"
$inf = Get-Content -LiteralPath "$package/AmtPtpDevice.inf" -Raw
$match = [regex]::Match($inf, '(?mi)^DriverVer\s*=\s*[^,\r\n]+,\s*(?<version>\d+\.\d+\.\d+\.\d+)\s*$')
if (-not $match.Success) {
    throw "Missing stamped DriverVer in $package"
}
$expected = $match.Groups['version'].Value
$drivers = @(Get-ChildItem -LiteralPath $package -Filter '*.sys' -File)
if ($drivers.Count -ne 4) {
    throw "Expected four SYS files in $package, found $($drivers.Count)"
}

foreach ($driver in $drivers) {
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($driver.FullName)
    $fileVersion = "$($info.FileMajorPart).$($info.FileMinorPart).$($info.FileBuildPart).$($info.FilePrivatePart)"
    $productVersion = "$($info.ProductMajorPart).$($info.ProductMinorPart).$($info.ProductBuildPart).$($info.ProductPrivatePart)"
    foreach ($version in $fileVersion, $productVersion, $info.FileVersion, $info.ProductVersion) {
        if ($version -ne $expected) {
            throw "$($driver.Name) version '$version' differs from INF DriverVer $expected"
        }
    }
    if ($info.OriginalFilename -cne $driver.Name -or $info.InternalName -cne $driver.Name) {
        throw "VERSIONINFO names do not match $($driver.Name)"
    }
    Write-Output "$($driver.Name) VERSIONINFO matches DriverVer $expected"
}
