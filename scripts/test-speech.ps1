[CmdletBinding()]
param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\target\release\voice-control.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\tests\audio'),
    [string]$Voice = 'Microsoft Irina Desktop',
    [switch]$GenerateOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Synthesized Russian fixtures exercise decoding, speech recognition and command
# routing together. They do not substitute for a live microphone/real-speaker test.
Add-Type -AssemblyName System.Speech
$null = New-Item -ItemType Directory -Path $OutputDirectory -Force
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$cases = @(
    [pscustomobject]@{ Id = 'play'; Text = 'играй'; Expected = 'play' }
    [pscustomobject]@{ Id = 'pause'; Text = 'пауза'; Expected = 'pause' }
    [pscustomobject]@{ Id = 'mute'; Text = 'мут'; Expected = 'mute' }
    [pscustomobject]@{ Id = 'unmute'; Text = 'размут'; Expected = 'unmute' }
    [pscustomobject]@{ Id = 'unmute-phrase'; Text = 'включи микрофон'; Expected = 'unmute' }
    [pscustomobject]@{ Id = 'negative-greeting'; Text = 'привет как дела'; Expected = $null }
    [pscustomobject]@{ Id = 'negative-weather'; Text = 'сегодня хорошая погода'; Expected = $null }
    [pscustomobject]@{ Id = 'negative-word-boundary'; Text = 'мутант'; Expected = $null }
    [pscustomobject]@{ Id = 'negative-negation'; Text = 'не ставь на паузу'; Expected = $null }
    [pscustomobject]@{ Id = 'silence'; Text = ''; Expected = $null }
)

$synth = [System.Speech.Synthesis.SpeechSynthesizer]::new()
try {
    $synth.SelectVoice($Voice)
    $synth.Rate = 0
    $synth.Volume = 100
    $format = [System.Speech.AudioFormat.SpeechAudioFormatInfo]::new(
        16000,
        [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
        [System.Speech.AudioFormat.AudioChannel]::Mono
    )
    foreach ($case in $cases) {
        $path = Join-Path $OutputDirectory ($case.Id + '.wav')
        $case | Add-Member -NotePropertyName Path -NotePropertyValue $path
        if ($case.Id -eq 'silence') {
            # Two seconds of PCM silence in the same format as the speech files.
            $pcmLength = 16000 * 2 * 2
            $file = [System.IO.File]::Create($path)
            $writer = [System.IO.BinaryWriter]::new($file)
            try {
                $writer.Write([System.Text.Encoding]::ASCII.GetBytes('RIFF'))
                $writer.Write([int]($pcmLength + 36))
                $writer.Write([System.Text.Encoding]::ASCII.GetBytes('WAVEfmt '))
                $writer.Write([int]16)
                $writer.Write([short]1)
                $writer.Write([short]1)
                $writer.Write([int]16000)
                $writer.Write([int]32000)
                $writer.Write([short]2)
                $writer.Write([short]16)
                $writer.Write([System.Text.Encoding]::ASCII.GetBytes('data'))
                $writer.Write([int]$pcmLength)
                $writer.Write([byte[]]::new($pcmLength))
            }
            finally {
                $writer.Dispose()
                $file.Dispose()
            }
        }
        else {
            $synth.SetOutputToWaveFile($path, $format)
            $synth.Speak($case.Text)
            $synth.SetOutputToNull()
        }
    }
}
finally {
    $synth.Dispose()
}

$manifest = [ordered]@{
    generated_at = [DateTimeOffset]::Now.ToString('o')
    voice = $Voice
    format = 'PCM signed 16-bit mono, 16000 Hz'
    purpose = 'Offline decoder + recognizer + command-routing integration; no live controls'
    cases = $cases
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'manifest.json') -Encoding utf8
Write-Host "Generated $($cases.Count) Russian audio fixtures in $OutputDirectory"

if ($GenerateOnly) {
    return
}
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "Build the release executable first: cargo build --release. Not found: $Executable"
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$results = @()
foreach ($case in $cases) {
    # --recognize-file does not dispatch commands: no Insert press or media change.
    $stderrPath = Join-Path $OutputDirectory ($case.Id + '.stderr.txt')
    $raw = & $Executable --recognize-file $case.Path --json 2> $stderrPath
    $recognizerExit = $LASTEXITCODE
    $events = @()
    $failure = $null
    if ($recognizerExit -ne 0) {
        $failure = "Recognizer exited $recognizerExit`: $([System.IO.File]::ReadAllText($stderrPath))"
    }
    else {
        try {
            $json = ($raw -join "`n").Trim()
            if (-not ($json.StartsWith('[') -and $json.EndsWith(']'))) {
                throw 'The recognizer must return a JSON array, including [] for silence.'
            }
            $events = @(($json | ConvertFrom-Json))
        }
        catch {
            $failure = "Invalid recognizer JSON: $($_.Exception.Message)"
        }
    }
    $commands = @($events | Where-Object { $null -ne $_ -and $null -ne $_.command } | ForEach-Object { $_.command })
    if ($null -eq $failure) {
        if ($null -eq $case.Expected -and $commands.Count -ne 0) {
            $failure = "Unexpected action on negative speech: $($commands -join ', ')"
        }
        elseif ($null -ne $case.Expected -and ($commands.Count -ne 1 -or $commands[0] -ne $case.Expected)) {
            $failure = "Expected exactly one '$($case.Expected)' command, got [$($commands -join ', ')]"
        }
        elseif (@($events | Where-Object { $null -ne $_.command -and $_.confidence -lt 0.65 }).Count -ne 0) {
            $failure = 'A command was accepted below the required confidence threshold of 0.65.'
        }
    }
    $result = [pscustomobject]@{
        id = $case.Id
        input_text = $case.Text
        expected_command = $case.Expected
        passed = ($null -eq $failure)
        error = $failure
        events = $events
    }
    $results += $result
    $mark = if ($result.passed) { 'PASS' } else { 'FAIL' }
    Write-Host "$mark $($case.Id): expected=$($case.Expected), recognized=$($commands -join ', ')"
    if ($null -ne $failure) { Write-Host "  $failure" }
}

$failures = @($results | Where-Object { -not $_.passed })
$report = [ordered]@{
    tested_at = [DateTimeOffset]::Now.ToString('o')
    executable = $Executable
    executable_sha256 = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
    voice = $Voice
    live_side_effects = $false
    scope = 'Synthesized Russian WAV -> application recognizer -> command router'
    limitations = 'Does not validate live microphone capture, real-speaker accents/noise, Discord hotkey delivery or a media player.'
    passed = ($failures.Count -eq 0)
    passed_count = $results.Count - $failures.Count
    total_count = $results.Count
    results = $results
}
$reportPath = Join-Path $OutputDirectory 'report.json'
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $reportPath -Encoding utf8
Write-Host "$($report.passed_count)/$($report.total_count) passed. Report: $reportPath"
if ($failures.Count -ne 0) {
    throw "$($failures.Count) speech integration checks failed. See $reportPath"
}
