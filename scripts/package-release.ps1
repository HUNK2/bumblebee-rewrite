[CmdletBinding()]
param(
    [string]$BuildDirectory,
    [string]$OutputDirectory,
    [string[]]$PrivatePatterns = @()
)

$ErrorActionPreference = 'Stop'
$packageSource = Split-Path -Parent $PSScriptRoot
$packageParent = Split-Path -Parent $packageSource
$packageLeaf = Split-Path -Leaf $packageSource
if (-not $BuildDirectory) { $BuildDirectory = Join-Path $packageParent ($packageLeaf + '-Build') }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $packageParent ($packageLeaf + '-Artifacts') }
$packageOutput = [IO.Path]::GetFullPath($OutputDirectory)
if ($packageOutput -eq $packageSource -or $packageOutput.StartsWith($packageSource + '\', [StringComparison]::OrdinalIgnoreCase) -or $packageSource.StartsWith($packageOutput + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Choose an output directory separate from the source tree and its ancestors.'
}
if (Test-Path -LiteralPath $packageOutput) {
    if ((Get-Item -LiteralPath $packageOutput).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Output directory must not be a link.' }
}
$packageExecutable = Join-Path ([IO.Path]::GetFullPath($BuildDirectory)) 'release\bumblebee.exe'
$packageStage = Join-Path $packageOutput 'bumblebee-rewrite-v0.1.1-windows-x64'
$packageArchive = Join-Path $packageOutput 'bumblebee-rewrite-v0.1.1-windows-x64.zip'
$packageFiles = [ordered]@{
    'bumblebee.exe' = $packageExecutable
    'Start-Bumblebee.ps1' = Join-Path $PSScriptRoot 'Start-Bumblebee.ps1'
    'Start-Bumblebee.cmd' = Join-Path $PSScriptRoot 'Start-Bumblebee.cmd'
    'README.md' = Join-Path $packageSource 'docs\player-quickstart.md'
    'LICENSE' = Join-Path $packageSource 'LICENSE'
    'THIRD-PARTY-NOTICES.md' = Join-Path $packageSource 'docs\third-party-notices.md'
    'RELEASE-NOTES.md' = Join-Path $packageSource 'docs\release-notes.md'
}
foreach ($packageInput in $packageFiles.Values) {
    if (-not (Test-Path -LiteralPath $packageInput -PathType Leaf)) { throw "Missing reviewed package input: $packageInput" }
}
& (Join-Path $PSScriptRoot 'audit-release.ps1') -Root $packageSource -PrivatePatterns $PrivatePatterns
if (Test-Path -LiteralPath $packageStage) {
    if ((Get-Item -LiteralPath $packageStage).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Staging directory must not be a link.' }
    foreach ($packageExisting in (Get-ChildItem -LiteralPath $packageStage -Force)) {
        if ($packageExisting.PSIsContainer -or $packageExisting.Name -notin $packageFiles.Keys -or ($packageExisting.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Staging directory contains an unreviewed file or link.'
        }
    }
}
[IO.Directory]::CreateDirectory($packageStage) | Out-Null
foreach ($packageEntry in $packageFiles.GetEnumerator()) {
    Copy-Item -LiteralPath $packageEntry.Value -Destination (Join-Path $packageStage $packageEntry.Key) -Force
}
$packageInspector = Join-Path $PSScriptRoot 'release_artifacts.py'
& python $packageInspector package $packageStage $packageArchive
if ($LASTEXITCODE -ne 0) { throw 'ZIP creation failed.' }
$packageAuditArguments = @($packageInspector, 'audit', $packageExecutable, '--archive', $packageArchive)
foreach ($packagePattern in $PrivatePatterns) { $packageAuditArguments += @('--private-pattern', $packagePattern) }
& python @packageAuditArguments
if ($LASTEXITCODE -ne 0) { throw 'Executable/ZIP audit failed. Do not publish this package.' }
$packageDigest = (Get-FileHash -LiteralPath $packageArchive -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText(($packageArchive + '.sha256'), ($packageDigest + '  ' + (Split-Path -Leaf $packageArchive) + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Output $packageArchive
