# PowerShell wrapper — Windows equivalent of run.sh.
# Auto-detects librubberband and adds --features rubberband when present.
# Re-runs on exit code 75 (triggered by the in-app restart IPC).

Set-Location $PSScriptRoot

$features = ""
# Common Windows install paths via vcpkg or manual build.
$candidates = @(
    "C:\vcpkg\installed\x64-windows\bin\rubberband.dll",
    "C:\Program Files\rubberband\bin\rubberband.dll"
)
foreach ($p in $candidates) {
    if (Test-Path $p) { $features = "--features rubberband"; break }
}

while ($true) {
    $proc = Start-Process -FilePath "cargo" -ArgumentList "run --release $features" -NoNewWindow -Wait -PassThru
    if ($proc.ExitCode -eq 75) {
        Write-Host "Rebuilding and restarting..."
        Start-Sleep -Seconds 1
    } else {
        break
    }
}
