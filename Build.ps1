# Build a standalone native executable and a launcher that opens this project.
$ErrorActionPreference = 'Stop'
Push-Location -LiteralPath $PSScriptRoot
try {
    cargo build --release --locked --offline
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $outputPath = Join-Path $PSScriptRoot 'dist'
    New-Item -ItemType Directory -Path $outputPath -Force | Out-Null
    # Ship the licenses for assets embedded in the executable.
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'assets\fonts\MapleMono-OFL.txt') -Destination $outputPath
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'assets\fonts\Recursive-OFL.txt') -Destination $outputPath
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'assets\fonts\AlibabaPuHuiTi-3-Notice.txt') -Destination $outputPath
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'assets\icons\material\Material-Icon-Theme-MIT.txt') -Destination $outputPath
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'assets\icons\Leader-Icons-Licenses.txt') -Destination $outputPath
    # Stage the complete executable, preserving an in-use previous build and its live buffers.
    $targetPath = Join-Path $outputPath 'Nanobug.exe'
    $versionId = [Guid]::NewGuid().ToString('N')
    $stagedPath = Join-Path $outputPath ("Nanobug.new-$versionId.exe")
    $previousPath = Join-Path $outputPath ("Nanobug.previous-$versionId.exe")
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\nanobug.exe') -Destination $stagedPath
    $hadPrevious = Test-Path -LiteralPath $targetPath
    if ($hadPrevious) { Move-Item -LiteralPath $targetPath -Destination $previousPath }
    try { Move-Item -LiteralPath $stagedPath -Destination $targetPath } catch {
        if ($hadPrevious -and -not (Test-Path -LiteralPath $targetPath)) {
            Move-Item -LiteralPath $previousPath -Destination $targetPath
        }
        throw
    }
    $launcher = "@echo off`r`nstart `"`" `"%~dp0Nanobug.exe`" `"%~dp0..`"`r`n"
    Set-Content -LiteralPath (Join-Path $outputPath '启动 Nanobug.cmd') -Value $launcher -Encoding ascii
    Write-Output (Join-Path $outputPath 'Nanobug.exe')
} finally { Pop-Location }
