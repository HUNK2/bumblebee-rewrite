[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$GameDirectory,
    [ValidateSet(1, 2, 4)][int]$TextureScale = 4,
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$playRoot = $PSScriptRoot
$playInstall = (Resolve-Path -LiteralPath $GameDirectory -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $playInstall -PathType Container)) {
    throw 'Select the directory containing the original PC game install.'
}
foreach ($playRequired in @('bnxglobal.str', 'characters\bumblebee.str')) {
    if (-not (Test-Path -LiteralPath (Join-Path $playInstall $playRequired) -PathType Leaf)) {
        throw "Required original PC game data is missing: $playRequired. Select the full game install."
    }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Install Rust with rustup and Microsoft C++ build tools, then restart PowerShell.'
}
$playPreviousInstall = $env:TF2_GAME_DIR
$playPreviousTarget = $env:CARGO_TARGET_DIR
try {
    $env:TF2_GAME_DIR = $playInstall
    $env:CARGO_TARGET_DIR = Join-Path $playRoot 'target'
    $playArguments = @('run', '--manifest-path', (Join-Path $playRoot 'Cargo.toml'), '--locked', '-p', 'bumblebee', '--')
    if ($Check) { $playArguments += @('--check', 'bumblebee') }
    else { $playArguments += @('--texture-scale', "$TextureScale") }
    & cargo @playArguments
    if ($LASTEXITCODE -ne 0) { throw "Build or startup failed (exit code $LASTEXITCODE)." }
} finally {
    $env:TF2_GAME_DIR = $playPreviousInstall
    $env:CARGO_TARGET_DIR = $playPreviousTarget
}
