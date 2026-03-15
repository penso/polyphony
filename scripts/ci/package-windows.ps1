param(
  [Parameter(Mandatory = $true)][string]$Tag,
  [Parameter(Mandatory = $true)][string]$TargetTriple,
  [Parameter(Mandatory = $true)][string]$BinaryPath,
  [Parameter(Mandatory = $true)][string]$OutputDir
)

$ErrorActionPreference = 'Stop'
$AppName = 'polyphony'
$StagingDir = Join-Path $OutputDir "$AppName-$Tag-$TargetTriple"
$ArchivePath = Join-Path $OutputDir "$AppName-$Tag-$TargetTriple.zip"

$BinDir = Join-Path $StagingDir 'bin'
New-Item -Path $BinDir -ItemType Directory -Force | Out-Null
Copy-Item -Path $BinaryPath -Destination (Join-Path $BinDir "$AppName.exe") -Force
Copy-Item -Path README.md -Destination (Join-Path $StagingDir 'README.md') -Force
if (Test-Path LICENSE) {
  Copy-Item -Path LICENSE -Destination (Join-Path $StagingDir 'LICENSE') -Force
} elseif (Test-Path LICENSE.md) {
  Copy-Item -Path LICENSE.md -Destination (Join-Path $StagingDir 'LICENSE') -Force
} else {
  Write-Warning 'no LICENSE or LICENSE.md found, skipping license bundle'
}

if ($env:POLYPHONY_CHANGELOG_PATH) {
  if (Test-Path $env:POLYPHONY_CHANGELOG_PATH) {
    Copy-Item -Path $env:POLYPHONY_CHANGELOG_PATH -Destination (Join-Path $StagingDir 'CHANGELOG.md') -Force
    Write-Output "bundled changelog from $($env:POLYPHONY_CHANGELOG_PATH)"
  } else {
    Write-Warning "changelog not found at $($env:POLYPHONY_CHANGELOG_PATH), skipping bundle"
  }
} else {
  Write-Output 'note: POLYPHONY_CHANGELOG_PATH not set, skipping changelog bundle'
}

if (Test-Path $ArchivePath) {
  Remove-Item -Path $ArchivePath -Force
}
Compress-Archive -Path (Join-Path $StagingDir '*') -DestinationPath $ArchivePath

Write-Output $ArchivePath
