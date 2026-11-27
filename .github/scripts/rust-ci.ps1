#Requires -Version 7

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64', 'all')]
    [string]$Arch = 'all',

    # Also extract the WDK packaging tools (stampinf and Inf2Cat in bin\, InfVerif in tools\) for
    # `cargo xtask package`. They are host (x64) tools, taken from the x64 WDK package.
    [switch]$PackagingTools
)

$PSNativeCommandUseErrorActionPreference = $true

if (-not (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll') -PathType Leaf)) {
    throw "libclang.dll is missing from $env:LIBCLANG_PATH"
}

$kit = Join-Path $env:RUNNER_TEMP 'windows-kit'
$sdkLib = Join-Path $kit "Lib/$env:Version_Number"
New-Item -ItemType Directory -Force -Path $kit, $sdkLib | Out-Null

function Expand-KitPackage($PackageId, $Destination, [string[]]$Members) {
    $version = $env:KIT_PACKAGE_VERSION
    $file = "$PackageId.$version.nupkg"
    $archive = Join-Path $env:RUNNER_TEMP $file
    Write-Information -MessageData "Downloading $file" -InformationAction Continue
    curl.exe --fail --location --silent --show-error --retry 3 --output $archive `
        "https://api.nuget.org/v3-flatcontainer/$PackageId/$version/$file"
    tar.exe -xf $archive -C $Destination --strip-components 1 @Members
}

# wdk-build expects the SDK and WDK files under the same kit root.
Expand-KitPackage -PackageId 'microsoft.windows.sdk.cpp' -Destination $kit -Members @('c/Include', 'c/bin')
$arches = if ($Arch -eq 'all') { @('x64', 'arm64') } else { @($Arch) }
$toolMembers = @('c/bin', 'c/tools')
foreach ($a in $arches) {
    Expand-KitPackage -PackageId "microsoft.windows.sdk.cpp.$a" -Destination $sdkLib -Members @('c/um', 'c/ucrt')
    $wdkMembers = @('c/Include', 'c/Lib')
    if ($PackagingTools -and $a -eq 'x64') {
        $wdkMembers += $toolMembers
    }
    Expand-KitPackage -PackageId "microsoft.windows.wdk.$a" -Destination $kit -Members $wdkMembers
}
if ($PackagingTools -and 'x64' -notin $arches) {
    Expand-KitPackage -PackageId 'microsoft.windows.wdk.x64' -Destination $kit -Members $toolMembers
}
"WDKContentRoot=$kit" >> $env:GITHUB_ENV
