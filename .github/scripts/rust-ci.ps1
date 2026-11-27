#Requires -Version 7

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64', 'all')]
    [string]$Arch = 'all'
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
foreach ($a in $arches) {
    Expand-KitPackage -PackageId "microsoft.windows.sdk.cpp.$a" -Destination $sdkLib -Members @('c/um', 'c/ucrt')
    Expand-KitPackage -PackageId "microsoft.windows.wdk.$a" -Destination $kit -Members @('c/Include', 'c/Lib')
}
"WDKContentRoot=$kit" >> $env:GITHUB_ENV
