# Launch the start page from any working directory.
$ErrorActionPreference = 'Stop'
$projectPath = $PSScriptRoot
$programPath = Join-Path $projectPath 'dist\Nanobug.exe'
if (Test-Path -LiteralPath $programPath) {
    Start-Process -FilePath $programPath -WorkingDirectory $projectPath
} else {
    Push-Location -LiteralPath $projectPath
    try { cargo run --offline } finally { Pop-Location }
}
