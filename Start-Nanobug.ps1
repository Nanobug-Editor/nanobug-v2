# Launch from any working directory while opening this project by default.
$ErrorActionPreference = 'Stop'
$projectPath = $PSScriptRoot
$programPath = Join-Path $projectPath 'dist\Nanobug.exe'
if (Test-Path -LiteralPath $programPath) {
    Start-Process -FilePath $programPath -ArgumentList ('"' + $projectPath + '"') -WorkingDirectory $projectPath
} else {
    Push-Location -LiteralPath $projectPath
    try { cargo run --offline -- $projectPath } finally { Pop-Location }
}
