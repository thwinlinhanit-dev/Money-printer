# Orchestration script to run all components of the Money Printer trading system.

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "       RUNNING THE MONEY PRINTER SYSTEM END-TO-END" -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# 1. Verification of platform-independent tests in Cargo Workspace
Write-Host "`n[1/5] Running platform-independent Rust tests..." -ForegroundColor Yellow
cargo test --workspace --exclude mp-ops --offline
if ($LASTEXITCODE -ne 0) {
    Write-Host "Rust tests failed!" -ForegroundColor Red
    Exit 1
}
Write-Host "All Rust tests passed successfully!" -ForegroundColor Green

# 2. Event Log Data Generation
Write-Host "`n[2/5] Generating synthetic market event data..." -ForegroundColor Yellow
cargo run --bin gen_fixture -- demo.eventlog
if ($LASTEXITCODE -ne 0) {
    Write-Host "Data generation failed!" -ForegroundColor Red
    Exit 1
}

# 3. Simulation & Backtesting Engine Pipeline
Write-Host "`n[3/5] Running Simulator Pipeline (Backtest, Walk-Forward, Monte Carlo)..." -ForegroundColor Yellow

Write-Host "`n---> Running Backtester:" -ForegroundColor Cyan
cargo run --bin sim -- backtest --log demo.eventlog --strategy coinflip --seed 42 --run-id demo-run-101 --runs-dir runs
if ($LASTEXITCODE -ne 0) {
    Write-Host "Backtest failed!" -ForegroundColor Red
    Exit 1
}

Write-Host "`n---> Running Walk-Forward Validation:" -ForegroundColor Cyan
cargo run --bin sim -- wf --log demo.eventlog --strategy coinflip --seed 42 --train-ns 3600000000000 --test-ns 1800000000000 --step-ns 900000000000
if ($LASTEXITCODE -ne 0) {
    Write-Host "Walk-forward failed!" -ForegroundColor Red
    Exit 1
}

Write-Host "`n---> Running Monte Carlo Risk Profiling:" -ForegroundColor Cyan
cargo run --bin sim -- mc --log demo.eventlog --strategy coinflip --seed 42 --resamples 500
if ($LASTEXITCODE -ne 0) {
    Write-Host "Monte Carlo failed!" -ForegroundColor Red
    Exit 1
}

# 4. Strategy Lifecycle Funnel Management
Write-Host "`n[4/5] Running Strategy Lifecycle Funnel..." -ForegroundColor Yellow

Write-Host "`n---> Registering carry-v1 strategy (with complete hypothesis):" -ForegroundColor Cyan
cargo run --bin funnel -- demo_strategy.json register carry-v1 --hypothesis-complete
if ($LASTEXITCODE -ne 0) {
    Write-Host "Funnel register failed!" -ForegroundColor Red
    Exit 1
}

Write-Host "`n---> Promoting strategy to Hypothesis:" -ForegroundColor Cyan
cargo run --bin funnel -- demo_strategy.json promote hypothesis
if ($LASTEXITCODE -ne 0) {
    Write-Host "Funnel promote to hypothesis failed!" -ForegroundColor Red
    Exit 1
}

Write-Host "`n---> Promoting strategy to Backtest:" -ForegroundColor Cyan
cargo run --bin funnel -- demo_strategy.json promote backtest
if ($LASTEXITCODE -ne 0) {
    Write-Host "Funnel promote to backtest failed!" -ForegroundColor Red
    Exit 1
}

# 5. Python Research Intelligence Tests
Write-Host "`n[5/5] Running Python Research Intelligence Tests..." -ForegroundColor Yellow
py -m pytest research/tests/test_brief.py research/tests/test_coverage.py research/tests/test_event_study.py research/tests/test_grading.py -q
if ($LASTEXITCODE -ne 0) {
    Write-Host "Python research tests failed!" -ForegroundColor Red
    Exit 1
}
Write-Host "All Python research tests passed successfully!" -ForegroundColor Green

# Cleanup temporary files
Write-Host "`nCleaning up temporary demo files..." -ForegroundColor Yellow
Remove-Item demo.eventlog, demo_strategy.json, demo_strategy.json.journal -ErrorAction SilentlyContinue
Remove-Item -Recurse runs -ErrorAction SilentlyContinue

Write-Host "`n==========================================================" -ForegroundColor Green
Write-Host "     FULL SYSTEM RUN COMPLETE AND VERIFIED SUCCESSFULLY" -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Green
