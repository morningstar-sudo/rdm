param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-p]{32}$')]
    [string] $ChromeExtensionId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-p]{32}$')]
    [string] $EdgeExtensionId,

    [Parameter(Mandatory = $true)]
    [string] $InstallDirectory
)

$ErrorActionPreference = 'Stop'
$InstallDirectory = [System.IO.Path]::GetFullPath($InstallDirectory)
$hostPath = Join-Path $InstallDirectory 'rdm.exe'
$manifestPath = Join-Path $InstallDirectory 'com.rdm.download.json'

if (-not (Test-Path -LiteralPath $hostPath -PathType Leaf)) {
    throw "RDM executable not found: $hostPath"
}

$manifest = @{
    name = 'com.rdm.download'
    description = 'RDM browser URL handoff'
    path = $hostPath
    type = 'stdio'
    allowed_origins = @(
        "chrome-extension://$ChromeExtensionId/",
        "chrome-extension://$EdgeExtensionId/"
    )
} | ConvertTo-Json -Depth 3

[System.IO.File]::WriteAllText(
    $manifestPath,
    $manifest,
    (New-Object System.Text.UTF8Encoding($false))
)
foreach ($keyPath in @(
    'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.rdm.download',
    'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.rdm.download'
)) {
    New-Item -Path $keyPath -Force | Out-Null
    Set-Item -Path $keyPath -Value $manifestPath
}

Write-Output 'Native host registered for Chrome and Edge.'
$extensionDirectory = Join-Path (Split-Path $PSScriptRoot -Parent) 'apps\Rdm.Extension'
Write-Output "Load the unpacked extension from: $extensionDirectory"
