[CmdletBinding()]
param(
    [string]$GameDirectory,
    [ValidateSet(1, 2, 4)][int]$TextureScale = 4,
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$playerExecutable = Join-Path $PSScriptRoot 'bumblebee.exe'
if (-not (Test-Path -LiteralPath $playerExecutable -PathType Leaf)) {
    throw 'Extract the entire Windows download before starting it. bumblebee.exe must be beside this launcher.'
}
$playerSettingsDirectory = if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'Bumblebee' } else { Join-Path ([IO.Path]::GetTempPath()) 'Bumblebee' }
$playerSettingsFile = Join-Path $playerSettingsDirectory 'install-path.txt'
if (-not $GameDirectory) {
    if ($env:TF2_GAME_DIR) { $GameDirectory = $env:TF2_GAME_DIR }
    elseif (Test-Path -LiteralPath $playerSettingsFile -PathType Leaf) {
        $GameDirectory = [IO.File]::ReadAllText($playerSettingsFile).Trim()
    }
}
if (-not $GameDirectory) {
    Write-Host 'Bumblebee requires your own installed copy of Transformers: Revenge of the Fallen (PC).'
    Write-Host 'Enter its folder containing bnxglobal.str and characters. The original game will not be launched.'
    $GameDirectory = (Read-Host 'PC game install folder').Trim().Trim('"')
}
if (-not $GameDirectory) { throw 'The original PC game install is required.' }
$playerInstall = (Resolve-Path -LiteralPath $GameDirectory -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $playerInstall -PathType Container)) {
    throw 'Select the directory containing the original PC game install.'
}
foreach ($playerRequired in @('bnxglobal.str', 'characters\bumblebee.str')) {
    if (-not (Test-Path -LiteralPath (Join-Path $playerInstall $playerRequired) -PathType Leaf)) {
        throw "Required original PC data is missing: $playerRequired. Supply the full install. To change a saved location, use -GameDirectory <folder>."
    }
}
[IO.Directory]::CreateDirectory($playerSettingsDirectory) | Out-Null
[IO.File]::WriteAllText($playerSettingsFile, $playerInstall, (New-Object System.Text.UTF8Encoding($false)))
$playerPreviousInstall = $env:TF2_GAME_DIR
try {
    $env:TF2_GAME_DIR = $playerInstall
    $playerArguments = if ($Check) { @('--check', 'bumblebee') } else { @('--texture-scale', "$TextureScale") }
    & $playerExecutable @playerArguments
    if ($LASTEXITCODE -ne 0) { throw "Bumblebee stopped with exit code $LASTEXITCODE. See README.md for setup and troubleshooting." }
} finally {
    $env:TF2_GAME_DIR = $playerPreviousInstall
}
