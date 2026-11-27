#Requires -Version 7

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ScopeResult,

    [Parameter(Mandatory)]
    [string]$HostResult,

    [Parameter(Mandatory)]
    [string]$DriversResult
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$results = [ordered]@{
    scope   = $ScopeResult
    host    = $HostResult
    drivers = $DriversResult
}

$failed = @()
foreach ($entry in $results.GetEnumerator()) {
    Write-Information -MessageData "$($entry.Key): $($entry.Value)" -InformationAction Continue
    if ($entry.Value -notin @('success', 'skipped')) {
        $failed += "$($entry.Key)=$($entry.Value)"
    }
}

if ($failed.Count -gt 0) {
    throw "Rust CI failed: $($failed -join ', ')"
}
