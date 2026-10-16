# Package an already-built portable development binary. Never signs or publishes.
param([string]$Target = 'x86_64-pc-windows-msvc')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or $Target -ne 'x86_64-pc-windows-msvc') {
    throw 'Run with PowerShell 7 on Windows for x86_64-pc-windows-msvc.'
}
Set-Location (Join-Path $PSScriptRoot '..')
$Binary = "target/$Target/release/space.exe"
$Inventory = "target/dependency-inventory-$Target.json"
if (-not (Test-Path $Binary) -or -not (Test-Path $Inventory)) {
    throw 'Build the release binary and generate the dependency inventory first.'
}
New-Item -ItemType Directory -Force -Path dist | Out-Null
$Stage = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
$Name = "space-$Target-g0"
$Root = Join-Path $Stage $Name
try {
    New-Item -ItemType Directory -Path "$Root/docs" -Force | Out-Null
    Copy-Item $Binary "$Root/space.exe"
    Copy-Item LICENSE "$Root/LICENSE"
    Copy-Item docs/platform-support.md, docs/dependencies.md, docs/development.md "$Root/docs/"
    Copy-Item $Inventory "$Root/dependency-inventory.json"
    $Revision = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve source revision.' }
    $Compiler = rustc -Vv
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify compiler.' }
    $Version = & $Binary --version
    if ($LASTEXITCODE -ne 0) { throw 'Binary version smoke failed.' }
    @('Space G0 development package; not a validated release', "Target: $Target",
      "Revision: $Revision", 'Signing: unsigned; no Authenticode signature',
      $Compiler, $Version) | Set-Content "$Root/BUILD-INFO.txt" -Encoding utf8
    Compress-Archive -Path $Root -DestinationPath "dist/$Name.zip" -Force
    # Upload only evidence. Distribution of the executable/archive remains blocked
    # until third-party licenses and NOTICE texts have been assembled and reviewed.
    $Evidence = "target/package-evidence-$Target"
    New-Item -ItemType Directory -Path $Evidence -Force | Out-Null
    Copy-Item "$Root/BUILD-INFO.txt" "$Evidence/BUILD-INFO.txt"
    Write-Output "Created local-only development archive (distribution blocked): dist/$Name.zip"
} finally {
    if (Test-Path $Stage) { Remove-Item $Stage -Recurse -Force }
}
