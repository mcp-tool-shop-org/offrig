# Local gate: formatting, lints, tests, and a smoke run of both binaries.
# Run from anywhere; exits non-zero on the first failure. No network, no RunPod.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Step([string]$Name, [scriptblock]$Body) {
    Write-Host "`n==> $Name"
    & $Body
    if ($LASTEXITCODE -ne 0) {
        Write-Host "verify: FAILED at '$Name' (exit $LASTEXITCODE)" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Step 'cargo fmt --check' { cargo fmt --all -- --check }
Step 'cargo clippy' { cargo clippy --workspace --all-targets --locked -- -D warnings }
Step 'cargo test' { cargo test --workspace --locked }
Step 'smoke: offrig --help' { cargo run --quiet --locked -p offrig-cli -- --help | Out-Null }
Step 'smoke: offrig --version' { cargo run --quiet --locked -p offrig-cli -- --version }
Step 'smoke: offrig-mcp --help' { cargo run --quiet --locked -p offrig-mcp -- --help | Out-Null }

Write-Host "`nverify: all checks passed"
