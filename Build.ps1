# Build the Windows editor and package its portable release.
$ErrorActionPreference = 'Stop'
$restartEditor = $false
Push-Location -LiteralPath $PSScriptRoot
try {
    cargo build --release --locked --offline -p nanobug -p nanobug-plugin-runtime
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $outputPath = Join-Path $PSScriptRoot 'dist'
    New-Item -ItemType Directory -Path $outputPath -Force | Out-Null
    $versionId = [Guid]::NewGuid().ToString('N')
    $stagePath = Join-Path $outputPath "package.new-$versionId"
    $targetPath = Join-Path $outputPath 'Nanobug.exe'
    $runtimePath = Join-Path $outputPath 'nanobug-plugin-runtime.exe'
    $stagedPath = Join-Path $stagePath 'Nanobug.exe'
    $runtimeStage = Join-Path $stagePath 'nanobug-plugin-runtime.exe'
    $launcherStage = Join-Path $stagePath '启动 Nanobug.cmd'
    $noticesStage = Join-Path $stagePath 'THIRD-PARTY-NOTICES.md'
    $archiveStage = Join-Path $stagePath 'Nanobug-Windows-Preview.zip'
    $releaseFiles = @($stagedPath, $runtimeStage, $launcherStage, $noticesStage)

    New-Item -ItemType Directory -Path $stagePath | Out-Null
    try {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\nanobug.exe') -Destination $stagedPath
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\nanobug-plugin-runtime.exe') -Destination $runtimeStage
        $launcher = "@echo off`r`nstart `"`" `"%~dp0Nanobug.exe`"`r`n"
        Set-Content -LiteralPath $launcherStage -Value $launcher -Encoding ascii

        # Keep the original notices with their assets and include the full text in each release.
        $noticeSources = [ordered]@{
            'Alibaba PuHuiTi 3.0' = 'assets\fonts\AlibabaPuHuiTi-3-Notice.txt'
            'Maple Mono'          = 'assets\fonts\MapleMono-OFL.txt'
            'Recursive'           = 'assets\fonts\Recursive-OFL.txt'
            'Material Icon Theme' = 'assets\icons\material\Material-Icon-Theme-MIT.txt'
            'Leader hint icons'   = 'assets\icons\Leader-Icons-Licenses.txt'
            'English Hunspell dictionary (SCOWL)' = 'assets\spelling\en_US-LICENSE.txt'
            '[Spellbook 0.4.2 source](https://crates.io/api/v1/crates/spellbook/0.4.2/download) (MPL-2.0)' = 'assets\spelling\Spellbook-MPL-2.0.txt'
        }
        $noticeSections = foreach ($source in $noticeSources.GetEnumerator()) {
            $notice = Get-Content -LiteralPath (Join-Path $PSScriptRoot $source.Value) -Raw -Encoding UTF8
            "## $($source.Key)`n`n~~~text`n$notice`n~~~"
        }
        $notices = (@('# Third-Party Notices') + $noticeSections) -join "`n`n"
        [IO.File]::WriteAllText($noticesStage, "$notices`n")

        # Package the staged files so earlier builds in dist cannot enter the ZIP.
        Compress-Archive -LiteralPath $releaseFiles -DestinationPath $archiveStage

        # Use the normal window close path so the editor can resolve unsaved changes before the swap.
        $sessionId = (Get-Process -Id $PID).SessionId
        $editors = Get-Process -Name 'Nanobug','Nanobug.previous-*' -ErrorAction SilentlyContinue |
            Where-Object {
                $editorPath = $_.Path
                $_.SessionId -eq $sessionId -and $editorPath -and
                    [IO.Path]::GetDirectoryName($editorPath) -eq $outputPath -and
                    [IO.Path]::GetFileName($editorPath) -match '^Nanobug(?:\.previous-[0-9a-f]{32})?\.exe$'
            }
        foreach ($editor in $editors) {
            if ($editor.HasExited) { continue }
            $closeRequested = $editor.CloseMainWindow()
            if (-not $closeRequested -and -not $editor.HasExited) {
                throw "Could not close Nanobug (PID $($editor.Id)). The update was not installed."
            }
            Write-Host "Waiting for Nanobug (PID $($editor.Id)) to close. Confirm any prompts in its window."
            if (-not $editor.WaitForExit(60000)) {
                throw "Nanobug (PID $($editor.Id)) did not close within 60 seconds. The update was not installed."
            }
            $restartEditor = $true
        }

        # Keep the old executable pair available until the swap can no longer roll back.
        $previousPath = Join-Path $outputPath "Nanobug.previous-$versionId.exe"
        $runtimePrevious = Join-Path $outputPath "plugin-runtime.previous-$versionId.exe"
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
        foreach ($file in @($launcherStage, $noticesStage, $archiveStage)) {
            Move-Item -LiteralPath $file -Destination $outputPath -Force
        }

        # Remove only the loose notice files shipped by earlier builds.
        $legacyNotices = @($noticeSources.Values | Split-Path -Leaf) + 'Lucide-ISC.txt'
        foreach ($file in $legacyNotices) {
            $legacyPath = Join-Path $outputPath $file
            if (Test-Path -LiteralPath $legacyPath -PathType Leaf) {
                Remove-Item -LiteralPath $legacyPath
            }
        }
    } finally {
        foreach ($file in ($releaseFiles + $archiveStage)) {
            if (Test-Path -LiteralPath $file -PathType Leaf) {
                Remove-Item -LiteralPath $file
            }
        }
        [IO.Directory]::Delete($stagePath)
    }

    # Retry backups left by earlier builds once Windows releases their executables.
    foreach ($backup in Get-ChildItem -LiteralPath $outputPath -File) {
        if ($backup.Name -match '^(?:Nanobug|plugin-runtime)\.previous-([0-9a-f]{32})\.exe$') {
            # A concurrent build can still need its backups while its staging directory exists.
            $backupStage = Join-Path $outputPath "package.new-$($Matches[1])"
            if (Test-Path -LiteralPath $backupStage -PathType Container) { continue }

            try {
                Remove-Item -LiteralPath $backup.FullName
            } catch [System.Management.Automation.ItemNotFoundException] {
                # Another build already removed this backup.
            } catch {
                Write-Warning "Could not remove '$($backup.FullName)'. Cleanup will retry on the next successful build. $_"
            }
        }
    }
} finally {
    Pop-Location
    # Reopen the installed editor, including when a failed swap restored the previous version.
    if ($restartEditor -and (Test-Path -LiteralPath $targetPath -PathType Leaf)) {
        Start-Process -FilePath $targetPath -WorkingDirectory $PSScriptRoot
    }
}
Write-Output $targetPath
