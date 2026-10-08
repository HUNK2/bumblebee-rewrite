[CmdletBinding()]
param([string]$BuildDirectory)

$ErrorActionPreference = 'Stop'
$releaseRoot = Split-Path -Parent $PSScriptRoot
if (-not $BuildDirectory) {
    $BuildDirectory = Join-Path (Split-Path -Parent $releaseRoot) ((Split-Path -Leaf $releaseRoot) + '-Build')
}
$releaseBuild = [IO.Path]::GetFullPath($BuildDirectory)
if ($releaseBuild -eq $releaseRoot -or $releaseRoot.StartsWith($releaseBuild + '\', [StringComparison]::OrdinalIgnoreCase) -or $releaseBuild.StartsWith($releaseRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Choose a build-output directory separate from the source tree and its ancestors.'
}
$releasePreviousTarget = $env:CARGO_TARGET_DIR
$releasePreviousFlags = $env:RUSTFLAGS
$releasePreviousEncodedFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $env:CARGO_TARGET_DIR = $releaseBuild
    $releaseFlags = @("--remap-path-prefix=$releaseRoot=source", "--remap-path-prefix=$($releaseRoot.Replace('\', '/'))=source")
    if ($env:USERPROFILE) {
        $releaseFlags += @("--remap-path-prefix=$env:USERPROFILE=build-user", "--remap-path-prefix=$($env:USERPROFILE.Replace('\', '/'))=build-user")
    }
    $env:CARGO_ENCODED_RUSTFLAGS = $releaseFlags -join [char]31
    & cargo build --manifest-path (Join-Path $releaseRoot 'Cargo.toml') --locked --offline --release -p bumblebee
    if ($LASTEXITCODE -ne 0) { throw "Release build failed (exit code $LASTEXITCODE)." }
    Write-Output (Join-Path $releaseBuild 'release\bumblebee.exe')
} finally {
    $env:CARGO_TARGET_DIR = $releasePreviousTarget
    $env:RUSTFLAGS = $releasePreviousFlags
    $env:CARGO_ENCODED_RUSTFLAGS = $releasePreviousEncodedFlags
}
