[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$LegacyCatalog,

    [Parameter(Mandatory = $true)]
    [string]$CandidateCatalog,

    [Parameter(Mandatory = $true)]
    [string]$MigrationReport,

    [Parameter(Mandatory = $true)]
    [string]$AcceptanceGate,

    [Parameter(Mandatory = $true)]
    [string]$PathAuditReport,

    [Parameter(Mandatory = $true)]
    [string]$HttpBinary,

    [Parameter(Mandatory = $true)]
    [string]$PathAuditBinary,

    [ValidateRange(1, 65535)]
    [int]$Port = 5000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function New-Check {
    param(
        [string]$Name,
        [bool]$Passed,
        [string]$Detail
    )

    [pscustomobject]@{
        name = $Name
        passed = $Passed
        detail = $Detail
    }
}

function Get-NormalizedFullPath {
    param([string]$Path)

    $portable = if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        '\\' + $Path.Substring(8)
    } elseif ($Path.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
        $Path.Substring(4)
    } else {
        $Path
    }
    [IO.Path]::GetFullPath($portable).TrimEnd('\')
}

function Test-SamePath {
    param(
        [string]$First,
        [string]$Second
    )

    (Get-NormalizedFullPath $First).Equals(
        (Get-NormalizedFullPath $Second),
        [StringComparison]::OrdinalIgnoreCase
    )
}

function Get-CatalogSidecars {
    param([string]$Path)

    @('-wal', '-shm', '-journal' | ForEach-Object { "$Path$_" } | Where-Object {
        Test-Path -LiteralPath $_
    })
}

function Invoke-PathAudit {
    param(
        [string]$Binary,
        [string]$Catalog
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Binary
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    $startInfo.StandardOutputEncoding = [Text.Encoding]::UTF8
    $startInfo.StandardErrorEncoding = [Text.Encoding]::UTF8
    $startInfo.ArgumentList.Add($Catalog)

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw '無法啟動 current path audit。'
    }
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "current path audit 失敗（exit $($process.ExitCode)）：$stderr"
    }
    $stdout | ConvertFrom-Json
}

$legacyPath = (Get-Item -LiteralPath $LegacyCatalog -ErrorAction Stop).FullName
$candidatePath = (Get-Item -LiteralPath $CandidateCatalog -ErrorAction Stop).FullName
$migrationPath = (Get-Item -LiteralPath $MigrationReport -ErrorAction Stop).FullName
$gatePath = (Get-Item -LiteralPath $AcceptanceGate -ErrorAction Stop).FullName
$savedAuditPath = (Get-Item -LiteralPath $PathAuditReport -ErrorAction Stop).FullName
$httpPath = (Get-Item -LiteralPath $HttpBinary -ErrorAction Stop).FullName
$auditBinaryPath = (Get-Item -LiteralPath $PathAuditBinary -ErrorAction Stop).FullName

$migration = Get-Content -LiteralPath $migrationPath -Raw | ConvertFrom-Json
$gate = Get-Content -LiteralPath $gatePath -Raw | ConvertFrom-Json
$savedAudit = Get-Content -LiteralPath $savedAuditPath -Raw | ConvertFrom-Json
$legacyHashBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath $legacyPath).Hash
$candidateHashBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidatePath).Hash
$freshAudit = Invoke-PathAudit -Binary $auditBinaryPath -Catalog $candidatePath
$legacyHashAfter = (Get-FileHash -Algorithm SHA256 -LiteralPath $legacyPath).Hash
$candidateHashAfter = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidatePath).Hash

$projectRoot = (Get-Item -LiteralPath (Split-Path -Parent $PSScriptRoot)).FullName
$projectPattern = [regex]::Escape($projectRoot)
$rustProcesses = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -eq 'doujin-http.exe'
})
$legacyProcesses = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -match '^pythonw?\.exe$' -and
    $_.CommandLine -match $projectPattern -and
    $_.CommandLine -match 'app\.py'
})
$listeners = @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
$legacySidecars = @(Get-CatalogSidecars $legacyPath)
$candidateSidecars = @(Get-CatalogSidecars $candidatePath)
$failedGateChecks = @($gate.checks | Where-Object { -not [bool]$_.passed })

$settings = $migration.reapply_setting_values
$viewerPath = [string]$settings.viewer_path
$thumbnailSize = [string]$settings.thumb_size
$thumbnailQuality = 0
$qualityValid = [int]::TryParse([string]$settings.thumb_quality, [ref]$thumbnailQuality) -and
    $thumbnailQuality -ge 1 -and $thumbnailQuality -le 100
$sizeValid = $thumbnailSize -match '^([1-9][0-9]{0,3})x([1-9][0-9]{0,3})$' -and
    [int]$Matches[1] -le 4096 -and [int]$Matches[2] -le 4096

$checks = @(
    New-Check 'catalog_paths_are_distinct' (-not (Test-SamePath $legacyPath $candidatePath)) "legacy=$legacyPath; candidate=$candidatePath"
    New-Check 'legacy_sha256_matches_gate' ($legacyHashBefore -eq [string]$gate.source_sha256) "actual=$legacyHashBefore; gate=$($gate.source_sha256)"
    New-Check 'legacy_copy_sha256_matches' ($legacyHashBefore -eq [string]$gate.source_copy_sha256) "legacy=$legacyHashBefore; copy=$($gate.source_copy_sha256)"
    New-Check 'candidate_sha256_matches_gate' ($candidateHashBefore -eq [string]$gate.target_sha256) "actual=$candidateHashBefore; gate=$($gate.target_sha256)"
    New-Check 'legacy_unchanged_during_preflight' ($legacyHashBefore -eq $legacyHashAfter) "before=$legacyHashBefore; after=$legacyHashAfter"
    New-Check 'candidate_unchanged_during_preflight' ($candidateHashBefore -eq $candidateHashAfter) "before=$candidateHashBefore; after=$candidateHashAfter"
    New-Check 'legacy_sidecars_absent' ($legacySidecars.Count -eq 0) "count=$($legacySidecars.Count)"
    New-Check 'candidate_sidecars_absent' ($candidateSidecars.Count -eq 0) "count=$($candidateSidecars.Count)"
    New-Check 'gate_passed' ($gate.status -eq 'passed' -and $failedGateChecks.Count -eq 0) "status=$($gate.status); failed_checks=$($failedGateChecks.Count)"
    New-Check 'gate_source_path' (Test-SamePath ([string]$gate.source_path) $legacyPath) "gate=$($gate.source_path); actual=$legacyPath"
    New-Check 'gate_target_path' (Test-SamePath ([string]$gate.target_path) $candidatePath) "gate=$($gate.target_path); actual=$candidatePath"
    New-Check 'migration_completed' ($migration.status -eq 'completed' -and [bool]$migration.source_fingerprint.unchanged) "status=$($migration.status); source_unchanged=$($migration.source_fingerprint.unchanged)"
    New-Check 'migration_target_path' (Test-SamePath ([string]$migration.target_path) $candidatePath) "report=$($migration.target_path); actual=$candidatePath"
    New-Check 'saved_path_audit_passed' ([bool]$savedAudit.passed) "passed=$($savedAudit.passed)"
    New-Check 'fresh_path_audit_passed' ([bool]$freshAudit.passed) "passed=$($freshAudit.passed)"
    New-Check 'collection_count_matches_path_audit' ([int]$migration.target_counts.collections -eq [int]$freshAudit.totals.current_paths) "catalog=$($migration.target_counts.collections); paths=$($freshAudit.totals.current_paths)"
    New-Check 'zip_count_matches_path_audit' ([int]$migration.target_counts.zip_collections -eq [int]$freshAudit.totals.existing_regular_zip) "catalog=$($migration.target_counts.zip_collections); paths=$($freshAudit.totals.existing_regular_zip)"
    New-Check 'image_folder_count_matches_path_audit' ([int]$migration.target_counts.image_folders -eq [int]$freshAudit.totals.existing_image_folder) "catalog=$($migration.target_counts.image_folders); paths=$($freshAudit.totals.existing_image_folder)"
    New-Check 'viewer_path_is_available' ([IO.Path]::IsPathFullyQualified($viewerPath) -and (Test-Path -LiteralPath $viewerPath -PathType Leaf)) "path=$viewerPath"
    New-Check 'thumbnail_size_is_valid' $sizeValid "value=$thumbnailSize"
    New-Check 'thumbnail_quality_is_valid' $qualityValid "value=$($settings.thumb_quality)"
    New-Check 'rust_server_is_stopped' ($rustProcesses.Count -eq 0) "count=$($rustProcesses.Count)"
    New-Check 'legacy_server_is_stopped' ($legacyProcesses.Count -eq 0) "count=$($legacyProcesses.Count)"
    New-Check 'cutover_port_is_free' ($listeners.Count -eq 0) "port=$Port; listeners=$($listeners.Count)"
)

$failedChecks = @($checks | Where-Object { -not $_.passed })
$result = [ordered]@{
    status = if ($failedChecks.Count -eq 0) { 'ready' } else { 'blocked' }
    legacy_catalog = $legacyPath
    legacy_sha256 = $legacyHashAfter
    candidate_catalog = $candidatePath
    candidate_sha256 = $candidateHashAfter
    http_binary = $httpPath
    http_binary_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $httpPath).Hash
    path_audit_binary = $auditBinaryPath
    path_audit_binary_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $auditBinaryPath).Hash
    port = $Port
    settings_to_reapply = [ordered]@{
        viewer_path = $viewerPath
        thumb_size = $thumbnailSize
        thumb_quality = $thumbnailQuality
    }
    current_paths = $freshAudit.totals
    checks = $checks
}

$result | ConvertTo-Json -Depth 8
if ($failedChecks.Count -gt 0) {
    exit 2
}
