param(
    [string]$Toolchain,
    [string]$NsisCompiler,
    [string]$Dumpbin,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$cargoArguments = @()
if ($Toolchain) { $cargoArguments += "+$Toolchain" }

Push-Location $repository
try {
    $metadataText = & cargo @cargoArguments metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
    $metadata = $metadataText | ConvertFrom-Json
    $package = $metadata.packages | Where-Object name -eq 'rust-transfer-gui' | Select-Object -First 1
    $version = $package.version
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'The installer requires a numeric major.minor.patch version.' }
    if (-not $SkipBuild) {
        & cargo @cargoArguments build --release --locked --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    }
    $binary = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc\release\rust-transfer-gui.exe'
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw "Release binary not found: $binary" }

    if (-not $NsisCompiler) {
        $command = Get-Command makensis -ErrorAction SilentlyContinue
        if ($command) { $NsisCompiler = $command.Source }
        else { $NsisCompiler = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }
    }
    if (-not (Test-Path -LiteralPath $NsisCompiler -PathType Leaf)) { throw 'Install NSIS 3 or pass -NsisCompiler.' }
    if (-not $Dumpbin) {
        $command = Get-Command dumpbin -ErrorAction SilentlyContinue
        if ($command) { $Dumpbin = $command.Source }
        else {
            $vswhere = @(
                (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'),
                (Join-Path $env:ProgramFiles 'Microsoft Visual Studio\Installer\vswhere.exe')
            ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
            if ($vswhere) {
                $Dumpbin = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe' | Select-Object -Last 1
            }
        }
    }
    if (-not $Dumpbin -or -not (Test-Path -LiteralPath $Dumpbin -PathType Leaf)) {
        throw 'Install Visual Studio C++ build tools or pass -Dumpbin for the runtime dependency check.'
    }
    $imports = & $Dumpbin /nologo /dependents $binary
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect release imports.' }
    $dependencies = @($imports | Where-Object { $_ -match '^\s*[A-Za-z0-9_.-]+\.dll\s*$' } | ForEach-Object { $_.Trim() })
    if ($dependencies.Count -eq 0) { throw 'No DLL import table was found.' }
    $externalRuntime = @($dependencies | Where-Object { $_ -match '^(vcruntime|msvcp|msvcr|concrt|vcomp|libssl|libcrypto|libssh2)' })
    if ($externalRuntime.Count -gt 0) { throw "Unexpected external runtime dependencies: $externalRuntime" }
    $headers = & $Dumpbin /nologo /headers $binary
    if ($LASTEXITCODE -ne 0 -or -not ($headers -match '8664 machine') -or -not ($headers -match '2 subsystem \(Windows GUI\)')) {
        throw 'Expected a Windows x64 GUI executable.'
    }

    $outputDirectory = Join-Path $repository 'dist'
    New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
    $installerScript = Join-Path $repository 'installer\windows\rust-transfer-gui.nsi'
    & $NsisCompiler /V3 /INPUTCHARSET UTF8 "/DVERSION=$version" "/DBINARY=$binary" "/DOUTDIR=$outputDirectory" $installerScript
    if ($LASTEXITCODE -ne 0) { throw 'NSIS compilation failed.' }
    $portable = Join-Path $outputDirectory "rust-transfer-gui-$version-x64-portable.exe"
    $installer = Join-Path $outputDirectory "rust-transfer-gui-$version-x64-setup.exe"
    Copy-Item -LiteralPath $binary -Destination $portable -Force
    $checksums = foreach ($artifact in @($portable, $installer)) {
        $hash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
        $name = Split-Path -Leaf $artifact
        "$hash  $name"
    }
    $checksumFile = Join-Path $outputDirectory "rust-transfer-gui-$version-SHA256SUMS.txt"
    $checksums | Set-Content -LiteralPath $checksumFile -Encoding ascii
    $dependencyFile = Join-Path $outputDirectory "rust-transfer-gui-$version-runtime-dependencies.txt"
    $dependencies | Set-Content -LiteralPath $dependencyFile -Encoding ascii
    Get-Item -LiteralPath $portable, $installer, $checksumFile, $dependencyFile | Select-Object Name, Length
}
finally {
    Pop-Location
}
