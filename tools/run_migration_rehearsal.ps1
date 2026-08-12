[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SourceCatalog,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [string]$CargoManifest = (Join-Path (Split-Path -Parent $PSScriptRoot) 'Cargo.toml'),

    [string]$TargetFileName = 'doujin-v2-rehearsal.db',

    [switch]$KeepSourceCopy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function New-GateCheck {
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

function Get-AppendedPath {
    param(
        [string]$Path,
        [string]$Suffix
    )

    return $Path + $Suffix
}

function Get-CatalogSidecars {
    param([string]$Path)

    return @(
        '-wal', '-shm', '-journal' | ForEach-Object {
            Get-AppendedPath -Path $Path -Suffix $_
        } | Where-Object { Test-Path -LiteralPath $_ }
    )
}

$sourceItem = Get-Item -LiteralPath $SourceCatalog
if ($sourceItem.PSIsContainer) {
    throw "來源必須是 SQLite 檔案：$($sourceItem.FullName)"
}

$sourcePath = $sourceItem.FullName
$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $outputPath) {
    throw "演練輸出目錄已存在，為避免覆寫而停止：$outputPath"
}
if (
    [string]::IsNullOrWhiteSpace($TargetFileName) -or
    [IO.Path]::GetFileName($TargetFileName) -ne $TargetFileName -or
    [IO.Path]::GetExtension($TargetFileName) -ne '.db'
) {
    throw "TargetFileName 必須是單一 .db 檔名：$TargetFileName"
}

$sourceSidecars = @(Get-CatalogSidecars -Path $sourcePath)
if ($sourceSidecars.Count -gt 0) {
    throw "來源 catalog 不是靜止副本，發現 sidecar：$($sourceSidecars -join '、')"
}

New-Item -ItemType Directory -Path $outputPath | Out-Null

$sourceCopyPath = Join-Path $outputPath 'legacy-source-copy.db'
$targetPath = Join-Path $outputPath $TargetFileName
$reportPath = Join-Path $outputPath 'migration-report.json'
$acceptancePath = Join-Path $outputPath 'acceptance-gate.json'

$sourceStream = [IO.File]::Open(
    $sourcePath,
    [IO.FileMode]::Open,
    [IO.FileAccess]::Read,
    [IO.FileShare]::Read
)
try {
    $sidecarsWhileLocked = @(Get-CatalogSidecars -Path $sourcePath)
    if ($sidecarsWhileLocked.Count -gt 0) {
        throw "取得來源唯讀鎖後發現 sidecar：$($sidecarsWhileLocked -join '、')"
    }

    $sourceHashBefore = (Get-FileHash -Algorithm SHA256 -InputStream $sourceStream).Hash
    $sourceStream.Position = 0
    $copyStream = [IO.File]::Open(
        $sourceCopyPath,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $sourceStream.CopyTo($copyStream)
        $copyStream.Flush($true)
    } finally {
        $copyStream.Dispose()
    }
    $sourceStream.Position = 0
    $sourceHashAfterCopy = (Get-FileHash -Algorithm SHA256 -InputStream $sourceStream).Hash
} finally {
    $sourceStream.Dispose()
}

$sidecarsAfterCopy = @(Get-CatalogSidecars -Path $sourcePath)
if ($sidecarsAfterCopy.Count -gt 0) {
    throw "來源複製完成後出現 sidecar，未執行 migration：$($sidecarsAfterCopy -join '、')"
}
$sourceCopyHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $sourceCopyPath).Hash
if ($sourceHashBefore -ne $sourceCopyHash -or $sourceHashBefore -ne $sourceHashAfterCopy) {
    throw '來源複製前後 SHA-256 不一致；未執行 migration。'
}

$manifestPath = (Get-Item -LiteralPath $CargoManifest).FullName
& cargo build --quiet --manifest-path $manifestPath -p doujin-migrate
if ($LASTEXITCODE -ne 0) {
    throw "doujin-migrate 建置失敗，exit code：$LASTEXITCODE"
}

$runnerPath = Join-Path (Split-Path -Parent $manifestPath) 'target\debug\doujin-migrate.exe'
if (-not (Test-Path -LiteralPath $runnerPath -PathType Leaf)) {
    throw "找不到 migration runner：$runnerPath"
}

$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $runnerPath
$startInfo.UseShellExecute = $false
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$startInfo.CreateNoWindow = $true
$startInfo.ArgumentList.Add($sourceCopyPath)
$startInfo.ArgumentList.Add($targetPath)

$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
if (-not $process.Start()) {
    throw '無法啟動 migration runner。'
}
$stdout = $process.StandardOutput.ReadToEnd()
$stderr = $process.StandardError.ReadToEnd()
$process.WaitForExit()
$runnerExitCode = $process.ExitCode

$utf8 = [Text.UTF8Encoding]::new($false)
if (-not [string]::IsNullOrWhiteSpace($stdout)) {
    [IO.File]::WriteAllText($reportPath, $stdout.TrimEnd() + [Environment]::NewLine, $utf8)
}

$report = $null
$reportParseError = $null
try {
    if (-not [string]::IsNullOrWhiteSpace($stdout)) {
        $report = $stdout | ConvertFrom-Json
    }
} catch {
    $reportParseError = $_.Exception.Message
}

$sourceHashAfterRun = (Get-FileHash -Algorithm SHA256 -LiteralPath $sourcePath).Hash
$copyHashAfterRun = (Get-FileHash -Algorithm SHA256 -LiteralPath $sourceCopyPath).Hash
$sourceSidecarsAfterRun = @(Get-CatalogSidecars -Path $sourcePath)
$checks = @(
    New-GateCheck 'source_sha256_unchanged' ($sourceHashBefore -eq $sourceHashAfterRun) "before=$sourceHashBefore; after=$sourceHashAfterRun"
    New-GateCheck 'source_copy_sha256_matches' ($sourceHashBefore -eq $copyHashAfterRun) "source=$sourceHashBefore; copy=$copyHashAfterRun"
    New-GateCheck 'source_sidecars_absent' ($sourceSidecarsAfterRun.Count -eq 0) "count=$($sourceSidecarsAfterRun.Count)"
    New-GateCheck 'runner_exit_code' ($runnerExitCode -eq 0) "exit_code=$runnerExitCode"
    New-GateCheck 'report_is_json' ($null -ne $report) $(if ($reportParseError) { $reportParseError } else { 'report parsed' })
)

if ($null -ne $report) {
    $checks += New-GateCheck 'migration_status' ($report.status -eq 'completed') "status=$($report.status)"
    $checks += New-GateCheck 'immutable_source_fingerprint' ([bool]$report.source_fingerprint.unchanged) "before=$($report.source_fingerprint.before_blake3); after=$($report.source_fingerprint.after_blake3)"
    $checks += New-GateCheck 'no_path_conflicts' (@($report.path_conflicts).Count -eq 0) "count=$(@($report.path_conflicts).Count)"
    $checks += New-GateCheck 'no_blocking_issues' (@($report.blocking_issues).Count -eq 0) "count=$(@($report.blocking_issues).Count)"
    $checks += New-GateCheck 'sqlite_integrity' ($report.validation.integrity_check -eq 'ok') "result=$($report.validation.integrity_check)"
    $checks += New-GateCheck 'foreign_keys' ([int]$report.validation.foreign_key_violations -eq 0) "violations=$($report.validation.foreign_key_violations)"
    $checks += New-GateCheck 'catalog_counts' (@($report.validation.count_mismatches).Count -eq 0) "mismatches=$(@($report.validation.count_mismatches).Count)"
    $checks += New-GateCheck 'zip_collection_count' ([int]$report.source_counts.zip_collections -eq [int]$report.target_counts.zip_collections) "source=$($report.source_counts.zip_collections); target=$($report.target_counts.zip_collections)"
    $checks += New-GateCheck 'image_folder_count' ([int]$report.source_counts.image_folders -eq [int]$report.target_counts.image_folders) "source=$($report.source_counts.image_folders); target=$($report.target_counts.image_folders)"
    $checks += New-GateCheck 'tag_names' ([int]$report.validation.tag_name_mismatches -eq 0) "mismatches=$($report.validation.tag_name_mismatches)"
    $checks += New-GateCheck 'tag_links' ([int]$report.validation.tag_link_mismatches -eq 0) "mismatches=$($report.validation.tag_link_mismatches)"
    $checks += New-GateCheck 'sample_metadata' (@($report.sample_metadata.mismatches).Count -eq 0) "checked=$($report.sample_metadata.checked); mismatches=$(@($report.sample_metadata.mismatches).Count)"

    foreach ($comparisonProperty in $report.effective_empty_value_comparison.PSObject.Properties) {
        $comparison = $comparisonProperty.Value
        $checks += New-GateCheck "empty_values_$($comparisonProperty.Name)" ($comparison.source -eq $comparison.target) "source=$($comparison.source); target=$($comparison.target)"
    }
}

$targetExists = Test-Path -LiteralPath $targetPath -PathType Leaf
$checks += New-GateCheck 'target_created' $targetExists "path=$targetPath"
$targetWal = Get-AppendedPath -Path $targetPath -Suffix '-wal'
$targetShm = Get-AppendedPath -Path $targetPath -Suffix '-shm'
$targetWalBytes = if (Test-Path -LiteralPath $targetWal) { (Get-Item -LiteralPath $targetWal).Length } else { 0 }
$checks += New-GateCheck 'target_wal_empty' ($targetWalBytes -eq 0) "bytes=$targetWalBytes"
$failedChecks = @($checks | Where-Object { -not $_.passed })
$gatePassed = $failedChecks.Count -eq 0

$targetHash = $null
if ($targetExists) {
    $targetHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $targetPath).Hash
}

$acceptance = [ordered]@{
    status = if ($gatePassed) { 'passed' } else { 'failed' }
    source_path = $sourcePath
    source_sha256 = $sourceHashBefore
    source_copy_sha256 = $sourceCopyHash
    target_path = $targetPath
    target_sha256 = $targetHash
    report_path = if (Test-Path -LiteralPath $reportPath) { $reportPath } else { $null }
    runner_exit_code = $runnerExitCode
    runner_stderr = if ([string]::IsNullOrWhiteSpace($stderr)) { $null } else { $stderr.Trim() }
    checks = $checks
}

[IO.File]::WriteAllText(
    $acceptancePath,
    ($acceptance | ConvertTo-Json -Depth 8) + [Environment]::NewLine,
    $utf8
)

if ($gatePassed) {
    foreach ($sidecar in @($targetWal, $targetShm)) {
        if (Test-Path -LiteralPath $sidecar) {
            [IO.File]::Delete($sidecar)
        }
    }
    if (-not $KeepSourceCopy) {
        [IO.File]::Delete($sourceCopyPath)
    }
    Write-Output ($acceptance | ConvertTo-Json -Depth 8)
    exit 0
}

Write-Error "Migration acceptance gate 未通過；請查看 $acceptancePath 與 $reportPath"
exit 2
