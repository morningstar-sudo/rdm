[CmdletBinding()]
param(
    [string] $InstallDirectory,

    [switch] $NoLaunch
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($InstallDirectory)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA is not set; pass -InstallDirectory explicitly.'
    }
    $InstallDirectory = Join-Path $env:LOCALAPPDATA 'Programs\RDM'
}

$InstallDirectory = [System.IO.Path]::GetFullPath($InstallDirectory)
$sourceExecutable = Join-Path $PSScriptRoot 'rdm.exe'
$sourceExtension = Join-Path $PSScriptRoot 'Rdm.Extension'
$sourceManifest = Join-Path $sourceExtension 'manifest.json'

if (-not (Test-Path -LiteralPath $sourceExecutable -PathType Leaf)) {
    throw "RDM executable not found: $sourceExecutable. Extract the complete release ZIP before running this installer."
}
if (-not (Test-Path -LiteralPath $sourceManifest -PathType Leaf)) {
    throw "Edge extension not found: $sourceManifest. Extract the complete release ZIP before running this installer."
}

New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
$installedExecutable = Join-Path $InstallDirectory 'rdm.exe'
$installedExtension = Join-Path $InstallDirectory 'Rdm.Extension'

$sourceExecutablePath = [System.IO.Path]::GetFullPath($sourceExecutable)
$installedExecutablePath = [System.IO.Path]::GetFullPath($installedExecutable)
if (-not $sourceExecutablePath.Equals(
        $installedExecutablePath,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
    Copy-Item -LiteralPath $sourceExecutable -Destination $installedExecutable -Force
}

New-Item -ItemType Directory -Force -Path $installedExtension | Out-Null
Get-ChildItem -LiteralPath $sourceExtension -Force | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $installedExtension -Recurse -Force
}

Write-Output "RDM installed to: $InstallDirectory"
if (-not $NoLaunch) {
    Start-Process -FilePath $installedExecutable
    Write-Output 'RDM started and will register the rdm:// protocol and Edge native messaging host.'
}
