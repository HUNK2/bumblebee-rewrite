[CmdletBinding()]
param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot),
    [string[]]$PrivatePatterns = @()
)

$ErrorActionPreference = 'Stop'
$auditRoot = (Resolve-Path -LiteralPath $Root).Path
$auditAllowed = @('.rs', '.wgsl', '.toml', '.lock', '.md', '.csv', '.ps1', '.py', '.cmd')
$auditSpecial = @('.gitignore', '.gitattributes', 'LICENSE')
$auditPatterns = @(
    '(?i)[A-Z]:[\\/]Users[\\/][^\s"''<>]+',
    '(?i)/(?:Users|home)/[^/\s"''<>]+',
    '(?i)[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}',
    '(?i)gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}',
    'BEGIN [A-Z ]*PRIVATE KEY'
) + $PrivatePatterns
$auditUtf8 = New-Object System.Text.UTF8Encoding($false, $true)
$auditFailures = New-Object 'System.Collections.Generic.List[string]'
$auditFiles = @(Get-ChildItem -LiteralPath $auditRoot -Recurse -File -Force |
    Where-Object { -not $_.FullName.StartsWith((Join-Path $auditRoot '.git') + '\', [StringComparison]::OrdinalIgnoreCase) })
foreach ($auditFile in $auditFiles) {
    $auditRelative = $auditFile.FullName.Substring($auditRoot.Length + 1)
    if ($auditFile.Extension -notin $auditAllowed -and $auditRelative -notin $auditSpecial) {
        $auditFailures.Add("Unreviewed file type: $auditRelative"); continue
    }
    $auditBytes = [IO.File]::ReadAllBytes($auditFile.FullName)
    if ($auditBytes -contains 0) { $auditFailures.Add("Binary content: $auditRelative"); continue }
    try { $auditText = $auditUtf8.GetString($auditBytes) }
    catch { $auditFailures.Add("Invalid UTF-8: $auditRelative"); continue }
    foreach ($auditPattern in $auditPatterns) {
        # Upstream contact/copyright notices are preserved; they are not project-owner identity.
        if ($auditRelative -eq 'docs\third-party-notices.md' -and $auditPattern -eq $auditPatterns[2]) { continue }
        if ($auditText -match $auditPattern -or $auditRelative -match $auditPattern) {
            # Do not print the private value itself.
            $auditFailures.Add("Potential identifying detail or credential: $auditRelative"); break
        }
    }
    if ($auditFile.Extension -eq '.csv') {
        foreach ($auditRow in ($auditText -split '\r?\n')) {
            if ($auditRow.Trim() -and $auditRow -notmatch '^[-+0-9.eE,\s]+$') {
                $auditFailures.Add("Non-numeric fixture content: $auditRelative"); break
            }
        }
    }
}
if ($auditFailures.Count) {
    $auditFailures | ForEach-Object { Write-Output $_ }
    throw "Release source audit failed: $($auditFailures.Count) finding(s)."
}
Write-Output "PASS: $($auditFiles.Count) source-tree files; no prohibited file types, binary payloads, user-profile paths, contact addresses or credential markers found."
Write-Output 'Numeric fixtures contain simulation observations only. Git metadata/history and future binaries require separate review.'
