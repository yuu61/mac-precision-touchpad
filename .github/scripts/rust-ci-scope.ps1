#Requires -Version 7

[CmdletBinding()]
param(
    [string]$EventName = $env:GITHUB_EVENT_NAME,
    [string]$BaseSha = '',
    [string]$HeadSha = 'HEAD',
    [string[]]$ChangedFiles = $null,
    [string]$GitHubOutput = $env:GITHUB_OUTPUT,
    [string]$GitHubStepSummary = $env:GITHUB_STEP_SUMMARY,
    [switch]$PassThru
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$ZeroSha = '0000000000000000000000000000000000000000'

$AllHostPackages = @(
    'ptp-hid',
    'bcm5974',
    'amtptp-usbkm-core',
    'amtptp-spikm-core',
    'amtptp-hidfilter-core',
    'xtask'
)
$CoreHostPackages = @(
    'ptp-hid',
    'bcm5974',
    'amtptp-usbkm-core',
    'amtptp-spikm-core',
    'amtptp-hidfilter-core'
)
$AllKmPackages = @(
    'amtptp-device-usb-km',
    'amtptp-device-spi-km',
    'amtptp-hid-filter',
    'amtptp-bth-filter'
)

function Test-GitCommit {
    param([string]$Ref)
    if ([string]::IsNullOrWhiteSpace($Ref) -or $Ref -eq $ZeroSha) {
        return $false
    }
    $null = & git cat-file -e "$Ref^{commit}" 2>$null
    return ($LASTEXITCODE -eq 0)
}

function Get-ChangedFilesFromGit {
    param(
        [string]$TriggerEvent,
        [string]$Base,
        [string]$Head
    )

    if ($TriggerEvent -eq 'workflow_dispatch') {
        return @{ Full = $true; Reason = 'workflow_dispatch'; Files = @() }
    }

    if ($TriggerEvent -eq 'pull_request') {
        # GitHub Actions checks out the PR merge commit (HEAD^1 = base, HEAD^2 = head).
        $null = & git rev-parse --verify -q 'HEAD^2' 2>$null
        if ($LASTEXITCODE -eq 0) {
            $files = @(& git -c core.quotepath=false diff --name-only --no-renames 'HEAD^1' 'HEAD')
            if ($LASTEXITCODE -eq 0) {
                return @{ Full = $false; Reason = 'pull_request merge diff (HEAD^1..HEAD)'; Files = $files }
            }
        }

        if (Test-GitCommit $Base) {
            $files = @(& git -c core.quotepath=false diff --name-only --no-renames "$Base...$Head")
            if ($LASTEXITCODE -eq 0) {
                return @{ Full = $false; Reason = "pull_request diff ($Base...$Head)"; Files = $files }
            }
        }

        return @{ Full = $true; Reason = 'pull_request base commit unavailable'; Files = @() }
    }

    if ($TriggerEvent -eq 'push') {
        if (-not (Test-GitCommit $Base)) {
            return @{ Full = $true; Reason = 'push base commit unavailable (new branch or force-push)'; Files = @() }
        }
        $files = @(& git -c core.quotepath=false diff --name-only --no-renames $Base $Head)
        if ($LASTEXITCODE -eq 0) {
            return @{ Full = $false; Reason = "push diff ($Base..$Head)"; Files = $files }
        }
        return @{ Full = $true; Reason = 'git diff failed on push'; Files = @() }
    }

    return @{ Full = $true; Reason = "unhandled event '$TriggerEvent'"; Files = @() }
}

function Add-All {
    param(
        [System.Collections.Generic.HashSet[string]]$Set,
        [string[]]$Items
    )
    foreach ($item in $Items) {
        [void]$Set.Add($item)
    }
}

$hostSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$kmSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$kmLintSupport = $false
# `cargo xtask package` (build, link, stamp, verify and catalog the driver package) runs whenever a
# driver is affected, and when the INF template or xtask changes.
$packageInputsChanged = $false

if ($null -ne $ChangedFiles) {
    $resolution = @{
        Full   = $false
        Reason = 'explicit ChangedFiles parameter'
        Files  = @($ChangedFiles)
    }
}
else {
    $resolution = Get-ChangedFilesFromGit -TriggerEvent $EventName -Base $BaseSha -Head $HeadSha
}

$normalizedFiles = @(
    $resolution.Files |
    ForEach-Object { ($_ -replace '\\', '/').Trim() -replace '^\./', '' } |
    Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
    Sort-Object -Unique
)

$fullRun = [bool]$resolution.Full
$fullReason = [string]$resolution.Reason

if ($fullRun) {
    Add-All $hostSet $AllHostPackages
    Add-All $kmSet $AllKmPackages
    $kmLintSupport = $true
}
else {
    foreach ($path in $normalizedFiles) {
        # Global Rust, Clippy, and CI configuration affects every workspace and support crate.
        if (
            $path -in @('.cargo/config.toml', 'clippy.toml', 'Cargo.toml', '.github/workflows/rust-ci.yml') -or
            $path -like '.github/scripts/rust-ci*.ps1'
        ) {
            $fullRun = $true
            $fullReason = "global config changed ($path)"
            Add-All $hostSet $AllHostPackages
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }

        if ($path -eq 'Cargo.lock') {
            Add-All $hostSet $AllHostPackages
            continue
        }

        # Shared core crates (tests/ only affects the host test/clippy of that crate).
        if ($path -like 'crates/ptp-hid/tests/*') {
            [void]$hostSet.Add('ptp-hid')
            continue
        }
        if ($path -like 'crates/ptp-hid/*') {
            Add-All $hostSet $CoreHostPackages
            # xtask's INF test reads the core crates' model tables (dev-dependencies).
            [void]$hostSet.Add('xtask')
            Add-All $kmSet $AllKmPackages
            continue
        }
        if ($path -like 'crates/bcm5974/*') {
            Add-All $hostSet @('bcm5974', 'amtptp-usbkm-core', 'amtptp-hidfilter-core', 'xtask')
            Add-All $kmSet @('amtptp-device-usb-km', 'amtptp-hid-filter', 'amtptp-bth-filter')
            continue
        }
        if ($path -like 'crates/amtptp-usbkm-core/tests/*') {
            [void]$hostSet.Add('amtptp-usbkm-core')
            continue
        }
        if ($path -like 'crates/amtptp-usbkm-core/*') {
            [void]$hostSet.Add('amtptp-usbkm-core')
            [void]$hostSet.Add('xtask')
            [void]$kmSet.Add('amtptp-device-usb-km')
            continue
        }
        if ($path -like 'crates/amtptp-spikm-core/tests/*') {
            [void]$hostSet.Add('amtptp-spikm-core')
            continue
        }
        if ($path -like 'crates/amtptp-spikm-core/*') {
            [void]$hostSet.Add('amtptp-spikm-core')
            [void]$hostSet.Add('xtask')
            [void]$kmSet.Add('amtptp-device-spi-km')
            continue
        }
        if ($path -like 'crates/amtptp-hidfilter-core/tests/*') {
            [void]$hostSet.Add('amtptp-hidfilter-core')
            continue
        }
        if ($path -like 'crates/amtptp-hidfilter-core/*') {
            [void]$kmSet.Add('amtptp-bth-filter')
            [void]$hostSet.Add('amtptp-hidfilter-core')
            [void]$hostSet.Add('xtask')
            [void]$kmSet.Add('amtptp-hid-filter')
            continue
        }
        if ($path -like 'crates/*') {
            Add-All $hostSet $AllHostPackages
            Add-All $kmSet $AllKmPackages
            continue
        }

        # xtask and the INF templates its tests check (SourceDisksFiles, model bindings).
        if ($path -like 'tools/xtask/*' -or $path -like 'packaging/*.inx') {
            [void]$hostSet.Add('xtask')
            $packageInputsChanged = $true
            continue
        }

        # Shared driver support crates used by the KMDF workspace.
        if ($path -like 'drivers/support/wdf-support/*') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }
        if ($path -like 'drivers/support/rc-embed/*') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }
        if ($path -like 'drivers/support/*') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }

        # KMDF driver workspace.
        if ($path -like 'drivers/km/AmtPtpDeviceUsbKm/*') {
            [void]$kmSet.Add('amtptp-device-usb-km')
            continue
        }
        if ($path -like 'drivers/km/AmtPtpDeviceSpiKm/*') {
            [void]$kmSet.Add('amtptp-device-spi-km')
            continue
        }
        if ($path -like 'drivers/km/AmtPtpBthFilter/*') {
            [void]$kmSet.Add('amtptp-bth-filter')
            continue
        }
        if ($path -like 'drivers/km/AmtPtpHidFilter/*') {
            [void]$hostSet.Add('amtptp-hidfilter-core')
            [void]$kmSet.Add('amtptp-hid-filter')
            continue
        }
        if ($path -eq 'drivers/km/Cargo.toml') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            # xtask verifies KMDF version in drivers/km/Cargo.toml.
            [void]$hostSet.Add('xtask')
            continue
        }
        if ($path -like 'drivers/km/*') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }

        if ($path -like 'drivers/*') {
            Add-All $kmSet $AllKmPackages
            $kmLintSupport = $true
            continue
        }
    }
}

$hostPackages = @($AllHostPackages | Where-Object { $hostSet.Contains($_) })
$kmPackages = @($AllKmPackages | Where-Object { $kmSet.Contains($_) })

$driverInclude = [System.Collections.Generic.List[object]]::new()
$arches = @(
    @{ Name = 'x64'; Target = 'x86_64-pc-windows-msvc' },
    @{ Name = 'arm64'; Target = 'aarch64-pc-windows-msvc' }
)

if ($kmPackages.Count -gt 0) {
    $kmPkgStr = $kmPackages -join ' '
    foreach ($a in $arches) {
        $driverInclude.Add([ordered]@{
                workspace    = 'km'
                label        = 'KMDF'
                arch         = $a.Name
                target       = $a.Target
                packages     = $kmPkgStr
                lint_support = $kmLintSupport
            })
    }
}

$runHost = $hostPackages.Count -gt 0
$runDrivers = $driverInclude.Count -gt 0
$runPackage = $runDrivers -or $packageInputsChanged
$hostPackagesStr = $hostPackages -join ' '
$driverMatrixObj = [ordered]@{ include = @($driverInclude.ToArray()) }
$driverMatrixJson = $driverMatrixObj | ConvertTo-Json -Compress -Depth 4

$hostSummary = if ($runHost) { '`' + $hostPackagesStr + '`' } else { 'skipped' }
$kmSummary = if ($kmPackages.Count -gt 0) {
    '`' + ($kmPackages -join ' ') + '`' + " (x64, arm64; support lint: $kmLintSupport)"
}
else {
    'skipped'
}
$packageSummary = if ($runPackage) { 'x64, arm64 (release, unsigned)' } else { 'skipped' }

$summaryLines = @(
    '### Rust CI Test Scope',
    '',
    "- **Resolution**: $fullReason",
    "- **Changed files**: $($normalizedFiles.Count)",
    "- **Host workspace**: $hostSummary",
    "- **KMDF drivers**: $kmSummary",
    "- **Driver package**: $packageSummary"
)

if (-not [string]::IsNullOrWhiteSpace($GitHubOutput)) {
    @(
        "run_host=$($runHost.ToString().ToLowerInvariant())",
        "host_packages=$hostPackagesStr",
        "run_drivers=$($runDrivers.ToString().ToLowerInvariant())",
        "driver_matrix=$driverMatrixJson",
        "run_package=$($runPackage.ToString().ToLowerInvariant())"
    ) | Out-File -FilePath $GitHubOutput -Encoding utf8 -Append
}

if (-not [string]::IsNullOrWhiteSpace($GitHubStepSummary)) {
    $summaryLines | Out-File -FilePath $GitHubStepSummary -Encoding utf8 -Append
}

if ($PassThru) {
    [pscustomobject]@{
        FullRun       = $fullRun
        Reason        = $fullReason
        ChangedFiles  = $normalizedFiles
        RunHost       = $runHost
        HostPackages  = $hostPackages
        RunDrivers    = $runDrivers
        KmPackages    = $kmPackages
        KmLintSupport = $kmLintSupport
        DriverMatrix  = $driverMatrixJson
        RunPackage    = $runPackage
    }
}
else {
    $summaryLines | ForEach-Object { Write-Information -MessageData $_ -InformationAction Continue }
}
