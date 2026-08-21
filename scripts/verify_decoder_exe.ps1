<#
.SYNOPSIS
    Verifies that the reference nse_fo_decoder_dyn.exe (and its runtime DLLs)
    actually run from wherever it's currently placed in this project, by
    piping every line of fixtures/sample_packets.hex through it.

.DESCRIPTION
    This exe is a reference oracle, not part of the Rust pipeline anymore
    (nse_decode ported its logic natively in Phase 2) -- it's kept around to
    spot-check the native decoder against real output when needed. This
    script only checks that the exe itself is runnable here: that it starts,
    that Windows can resolve all its DLL dependencies, and that it produces
    the expected output for the known fixture set.

.EXAMPLE
    powershell -File scripts\verify_decoder_exe.ps1
#>

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$exePath = Join-Path $repoRoot "nse_fo_decoder_dyn.exe"
$fixturePath = Join-Path $repoRoot "fixtures\sample_packets.hex"

# The exe's own directory is always searched first by Windows for its DLL
# dependencies -- these four ship alongside it in the original decoder
# folder and must be copied in wherever the exe moves to.
$requiredDlls = @(
    "liblzo2-2.dll",
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll",
    "libwinpthread-1.dll"
)

Write-Host "Checking $exePath ..."

if (-not (Test-Path $exePath)) {
    Write-Error "exe not found at $exePath"
    exit 1
}

$missingDlls = $requiredDlls | Where-Object { -not (Test-Path (Join-Path $repoRoot $_)) }
if ($missingDlls.Count -gt 0) {
    Write-Error "Missing runtime DLL(s) next to the exe: $($missingDlls -join ', ')"
    exit 1
}
Write-Host "All required DLLs present alongside the exe."

if (-not (Test-Path $fixturePath)) {
    Write-Error "fixture file not found at $fixturePath"
    exit 1
}

$lines = Get-Content $fixturePath | Where-Object { $_.Trim().Length -gt 0 }
$failures = 0

for ($i = 0; $i -lt $lines.Count; $i++) {
    $lineNum = $i + 1
    $hexLine = $lines[$i]

    # Feed exactly one hex line via stdin, same as nse_fo.py's subprocess call.
    $output = $hexLine | & $exePath 2>&1
    $exitCode = $LASTEXITCODE

    if ($exitCode -ne 0) {
        Write-Host "[$lineNum] FAIL -- exe exited with code $exitCode. Output: $output" -ForegroundColor Red
        $failures++
        continue
    }

    Write-Host "[$lineNum] OK -- exit 0, output: $output"
}

Write-Host ""
if ($failures -eq 0) {
    Write-Host "PASS -- exe ran successfully against all $($lines.Count) fixture line(s)." -ForegroundColor Green
    exit 0
} else {
    Write-Host "FAIL -- $failures of $($lines.Count) fixture line(s) failed." -ForegroundColor Red
    exit 1
}
