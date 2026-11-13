# Build the native editor, its plugin worker and a start page launcher.
$ErrorActionPreference = 'Stop'
Push-Location -LiteralPath $PSScriptRoot
try {
    cargo build --release --locked --offline -p nanobug -p nanobug-plugin-runtime
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
    $runtimePath = Join-Path $outputPath 'nanobug-plugin-runtime.exe'
    $runtimeStage = Join-Path $outputPath ("plugin-runtime.new-$versionId.exe")
    $runtimePrevious = Join-Path $outputPath ("plugin-runtime.previous-$versionId.exe")
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\nanobug-plugin-runtime.exe') -Destination $runtimeStage
    $hadPrevious = Test-Path -LiteralPath $targetPath
    $hadRuntime = Test-Path -LiteralPath $runtimePath
    $runtimeSwitched = $false
    try {
        if ($hadPrevious) { Move-Item -LiteralPath $targetPath -Destination $previousPath }
        if ($hadRuntime) { Move-Item -LiteralPath $runtimePath -Destination $runtimePrevious }
        Move-Item -LiteralPath $runtimeStage -Destination $runtimePath
        $runtimeSwitched = $true
        Move-Item -LiteralPath $stagedPath -Destination $targetPath
    } catch {
        if ($runtimeSwitched) { Move-Item -LiteralPath $runtimePath -Destination $runtimeStage }
        if ($hadRuntime -and (Test-Path -LiteralPath $runtimePrevious) -and -not (Test-Path -LiteralPath $runtimePath)) {
            Move-Item -LiteralPath $runtimePrevious -Destination $runtimePath
        }
        if ($hadPrevious -and -not (Test-Path -LiteralPath $targetPath)) {
            Move-Item -LiteralPath $previousPath -Destination $targetPath
        }
        throw
    }
    $launcher = "@echo off`r`nstart `"`" `"%~dp0Nanobug.exe`"`r`n"
    Set-Content -LiteralPath (Join-Path $outputPath '启动 Nanobug.cmd') -Value $launcher -Encoding ascii
    Write-Output (Join-Path $outputPath 'Nanobug.exe')
} finally { Pop-Location }
