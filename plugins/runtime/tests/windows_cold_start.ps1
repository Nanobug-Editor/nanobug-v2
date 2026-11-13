# Stage an isolated workload before restarting; measure it once per Windows boot.
param(
    [Parameter(Mandatory)] [string] $Fixture,
    [string] $Editor = '',
    [string] $Python = '',
    [switch] $Prepare,
    [ValidateRange(1, 100)] [int] $Rounds = 3
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
if (!$Editor) { $Editor = Join-Path $repo 'dist/Nanobug.exe' }
$Fixture = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Fixture)
$boot = (Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToFileTimeUtc()
$preparedBoot = Join-Path $Fixture 'prepared-boot.txt'
if ($Prepare) {
    $Editor = (Resolve-Path -LiteralPath $Editor).Path
    $worker = Join-Path (Split-Path -Parent $Editor) 'nanobug-plugin-runtime.exe'
    if (!(Test-Path -LiteralPath $worker -PathType Leaf)) { throw 'Put the runtime beside the editor.' }
    if (Test-Path -LiteralPath $Fixture) { throw 'Choose a new fixture directory.' }
    New-Item -ItemType Directory -Path $Fixture | Out-Null
    $bin = Join-Path $Fixture 'binaries'
    New-Item -ItemType Directory -Path $bin | Out-Null
    Copy-Item -LiteralPath $Editor -Destination (Join-Path $bin 'Nanobug.exe')
    Copy-Item -LiteralPath $worker -Destination $bin
    Get-FileHash -LiteralPath (Join-Path $bin 'Nanobug.exe'),(Join-Path $bin 'nanobug-plugin-runtime.exe') |
        Select-Object @{n='Name';e={Split-Path -Leaf $_.Path}},Hash |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Fixture 'binaries.json') -Encoding UTF8

    # Create packages through the real installer and populate the Wasmtime cache now.
    Push-Location -LiteralPath $repo
    $prior = $env:NANOBUG_PLUGIN_TEST_OUTPUT
    try {
        $env:NANOBUG_PLUGIN_TEST_OUTPUT = $Fixture
        cargo test --release --locked -p nanobug-plugin-runtime --test session prepare_launch_profiles -- --ignored --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'Could not prepare the 50 installed packages.' }
    } finally {
        $env:NANOBUG_PLUGIN_TEST_OUTPUT = $prior
        Pop-Location
    }
    $full = Join-Path $Fixture 'fifty'
    $cache = Join-Path $full 'compiled/modules'
    if (@(Get-ChildItem -LiteralPath $cache -File -Recurse).Count -lt 50) { throw 'Expected all 50 packages to have compiled entries.' }
    New-Item -ItemType Directory -Path (Join-Path $Fixture 'empty') | Out-Null
    for ($round = 1; $round -le $Rounds; $round++) {
        $miss = Join-Path $Fixture ('cold-missing-{0:00}' -f $round)
        New-Item -ItemType Directory -Path $miss | Out-Null
        Copy-Item -LiteralPath (Join-Path $full 'packages') -Destination $miss -Recurse
        Copy-Item -LiteralPath (Join-Path $full 'installed.json') -Destination $miss
    }
    Set-Content -LiteralPath $preparedBoot -Value $boot -Encoding ASCII
    Write-Output "Prepared $Rounds rounds at $Fixture. Restart Windows before the first sample."
    exit 0
}

if (!(Test-Path -LiteralPath $preparedBoot -PathType Leaf)) { throw 'Prepare the fixture before restarting.' }
if ([long](Get-Content -LiteralPath $preparedBoot -Raw) -ge $boot) {
    throw 'Windows is still on the boot that prepared this fixture. Restart before measuring.'
}
if (!$Python) {
    $bundledPython = Join-Path $env:USERPROFILE '.cache/codex-runtimes/codex-primary-runtime/dependencies/python/python.exe'
    $Python = if (Test-Path -LiteralPath $bundledPython -PathType Leaf) { $bundledPython } else { 'python' }
}
$Editor = Join-Path $Fixture 'binaries/Nanobug.exe'
& $Python (Join-Path $PSScriptRoot 'check_windows.py') $Fixture --editor $Editor --cold-boot $boot
exit $LASTEXITCODE
