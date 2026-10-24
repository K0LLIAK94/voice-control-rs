[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cache = Join-Path $root 'target\bootstrap'
New-Item -ItemType Directory -Force -Path $cache,(Join-Path $root 'runtime'),(Join-Path $root 'models') | Out-Null

function Get-VerifiedArchive([string]$Url, [string]$Path, [string]$Sha256) {
    if (!(Test-Path -LiteralPath $Path) -or (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Sha256) {
        Write-Host "Downloading $Url"
        Invoke-WebRequest -Uri $Url -OutFile $Path
    }
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Sha256) {
        throw "Checksum mismatch: $Path"
    }
}

# Official Vosk wheel. The SHA256 is published by PyPI's release metadata.
$wheel = Join-Path $cache 'vosk-0.3.45-win64.zip'
Get-VerifiedArchive 'https://files.pythonhosted.org/packages/c0/4c/deb0861f7da9696f8a255f1731bb73e9412cca29c4b3888a3fcb2a930a59/vosk-0.3.45-py3-none-win_amd64.whl' $wheel '6994DDC68556C7E5730C3B6F6BAD13320E3519B13CE3ED2AA25A86724E7C10AC'
Expand-Archive -LiteralPath $wheel -DestinationPath (Join-Path $cache 'wheel') -Force
foreach ($name in @('libvosk.dll','libgcc_s_seh-1.dll','libstdc++-6.dll','libwinpthread-1.dll')) {
    Copy-Item -LiteralPath (Join-Path $cache "wheel\vosk\$name") -Destination (Join-Path $root "runtime\$name") -Force
}
$model = Join-Path $cache 'vosk-model-small-ru-0.22.zip'
Get-VerifiedArchive 'https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip' $model '961D5FF98A17F4AA6DE69864D0AA71FA5BAC682301D2B5D17A3F24C5C99A46D4'
Expand-Archive -LiteralPath $model -DestinationPath (Join-Path $root 'models') -Force
Write-Host 'Vosk runtime and Russian model are ready. No Python installation is required.'
