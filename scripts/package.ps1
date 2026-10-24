[CmdletBinding()]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (!$SkipBuild) {
        cargo build --release --locked --bins
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    $bundle = Join-Path $root 'dist\VoiceControl'
    New-Item -ItemType Directory -Force -Path $bundle,(Join-Path $bundle 'runtime'),(Join-Path $bundle 'models'),(Join-Path $bundle 'docs'),(Join-Path $bundle 'assets'),(Join-Path $bundle 'analysis') | Out-Null
    foreach ($name in @('voice-control.exe','voice-control-cli.exe')) {
        Copy-Item -LiteralPath (Join-Path $root "target\release\$name") -Destination $bundle -Force
    }
    foreach ($name in @('libvosk.dll','libgcc_s_seh-1.dll','libstdc++-6.dll','libwinpthread-1.dll')) {
        Copy-Item -LiteralPath (Join-Path $root "runtime\$name") -Destination (Join-Path $bundle 'runtime') -Force
    }
    Copy-Item -LiteralPath (Join-Path $root 'models\vosk-model-small-ru-0.22') -Destination (Join-Path $bundle 'models') -Recurse -Force
    foreach ($name in @('README.md','LICENSE','THIRD_PARTY_NOTICES.md')) {
        Copy-Item -LiteralPath (Join-Path $root $name) -Destination $bundle -Force
    }
    # Preserve the portable installation's chosen microphone, vocabulary and UI zoom.
    if (!(Test-Path -LiteralPath (Join-Path $bundle 'config.toml'))) {
        Copy-Item -LiteralPath (Join-Path $root 'config.toml') -Destination $bundle
    }
    Copy-Item -LiteralPath (Join-Path $root 'analysis\laitis-report.md') -Destination (Join-Path $bundle 'docs\laitis-report.md') -Force
    Copy-Item -LiteralPath (Join-Path $root 'analysis\laitis-report.md') -Destination (Join-Path $bundle 'analysis\laitis-report.md') -Force
    Get-ChildItem -LiteralPath (Join-Path $root 'assets') -File | Copy-Item -Destination (Join-Path $bundle 'assets') -Force
    Copy-Item -LiteralPath (Join-Path $root 'docs\licenses') -Destination (Join-Path $bundle 'docs') -Recurse -Force
    Compress-Archive -Path (Join-Path $bundle '*') -DestinationPath (Join-Path $root 'dist\VoiceControl-windows-x64.zip') -Force
    Get-FileHash -LiteralPath (Join-Path $root 'dist\VoiceControl-windows-x64.zip') -Algorithm SHA256
}
finally { Pop-Location }
