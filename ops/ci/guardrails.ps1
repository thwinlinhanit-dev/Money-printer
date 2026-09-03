# guardrails.ps1 - Windows port of ops/ci/guardrails.sh (same rules, same
# exit contract). The bash version cannot run on the native Windows host that
# actually launches the collectors (audit 08-04 #6: mechanical PD enforcement
# was absent there); this shim restores it:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File ops/ci/guardrails.ps1
#
# Exit 0 = all checks passed; exit 1 = any violation ("GUARDRAIL FAIL:").
# Every check is a mechanical mirror of the bash version in guardrails.sh -
# keep the two in sync; do not weaken either to pass (PD-5). Keep checks
# fast, specific, low-false-positive; each names the rule it enforces.
#
# PowerShell 5.1-compatible (the operational host runs Windows PowerShell).

$ErrorActionPreference = "Stop"

# ---- workspace root = the Cargo.toml with a [workspace] section -------------
# Sub-crates (ops/, storage/, ...) also carry a Cargo.toml, so walk up until
# the one with a [workspace] section is found (same discovery as
# daily_pipeline.ps1).
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
while ($true) {
    $manifest = Join-Path $root "Cargo.toml"
    if ((Test-Path $manifest) -and (Select-String -Path $manifest -Pattern '^\[workspace\]' -Quiet)) { break }
    $parent = Split-Path $root -Parent
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found from $PSScriptRoot" -ForegroundColor Red; Exit 1 }
    $root = $parent
}
Push-Location $root

$script:fail = 0
function Err([string]$msg) {
    Write-Host "GUARDRAIL FAIL: $msg" -ForegroundColor Red
    $script:fail = 1
}

# All tracked files (git ls-files, the same `tracked` set the bash version
# uses). Array output; callers wrap in @(...).
function Get-TrackedAll {
    $out = & git ls-files 2>$null
    if ($LASTEXITCODE -ne 0) { return @() }
    return @($out)
}

# Tracked files matching pathspecs (e.g. '*.toml', 'core/**/*.rs').
function Get-TrackedPat([string[]]$Patterns) {
    $argList = @('ls-files', '--') + $Patterns
    $out = & git @argList 2>$null
    if ($LASTEXITCODE -ne 0) { return @() }
    return @($out)
}

# Read one tracked file's content, tolerating binary/unreadable files
# (the bash version's grep silently skips them too).
function Read-FileOrNull([string]$Path) {
    $full = Join-Path $root $Path
    if (-not (Test-Path -LiteralPath $full)) { return $null }
    if ((Get-Item -LiteralPath $full).PSIsContainer) { return $null }
    try { return Get-Content -LiteralPath $full -Raw -ErrorAction Stop } catch { return $null }
}

# ---- PD-2: no secrets, no real .env files -----------------------------------
foreach ($f in @(Get-TrackedAll)) {
    if ($f -match '(^|/)\.env$|(^|/)\.env\.[^e]' -and $f -notmatch '\.example') {
        Err "PD-2: committed env file: $f"
    }
}

foreach ($f in @(Get-TrackedAll)) {
    $c = Read-FileOrNull $f
    if ($null -eq $c) { continue }
    if ($c -cmatch '-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----') {
        Err "PD-2: private key material committed: $f"
    }
}

# Assigned, non-placeholder credentials in config-like files.
$credRe = '(?i)(api[_-]?key|api[_-]?secret|secret[_-]?key|access[_-]?token)\s*[:=]\s*"?[A-Za-z0-9+/_-]{16,}'
$credHits = @()
foreach ($f in @(Get-TrackedPat @('*.toml', '*.yaml', '*.yml', '*.json', '*.env.example'))) {
    $c = Read-FileOrNull $f
    if ($null -eq $c) { continue }
    foreach ($m in [regex]::Matches($c, $credRe, 'Multiline')) {
        if ($m.Value -match '(?i)example|placeholder|your[_-]|xxx|changeme|<[^>]+>') { continue }
        $credHits += "$f : $($m.Value)"
    }
}
if ($credHits.Count -gt 0) {
    foreach ($h in $credHits) { Write-Host $h -ForegroundColor Yellow }
    Err "PD-2: credential-looking value in tracked config (use *.example with placeholders)"
}

# Source-file credential scan (audit M-2): a key pasted into .rs/.ps1/.sh is
# never caught by the config scan above. Stricter pattern: the value must be a
# QUOTED string of length >= 16 after `key :=`, so env-var/identifier mentions
# (`env::var("API_KEY")`, `$env:API_KEY`) never match.
$srcCredRe = '(?i)(api[_-]?key|api[_-]?secret|secret[_-]?key|access[_-]?token|bot[_-]?token)\s*[:=]\s*"[^"]{16,}'
$srcCredHits = @()
foreach ($f in @(Get-TrackedPat @('*.rs', '*.ps1', '*.sh'))) {
    $c = Read-FileOrNull $f
    if ($null -eq $c) { continue }
    foreach ($m in [regex]::Matches($c, $srcCredRe, 'Multiline')) {
        if ($m.Value -match '(?i)example|placeholder|your[_-]|xxx|changeme|<[^>]+>') { continue }
        $srcCredHits += "$f : $($m.Value)"
    }
}
if ($srcCredHits.Count -gt 0) {
    foreach ($h in $srcCredHits) { Write-Host $h -ForegroundColor Yellow }
    Err "PD-2: credential-looking value in tracked source (use env vars + *.example)"
}

# ---- PD-2: no public IPv4 literals (audit M-1/M-2) --------------------------
# Match only 4-octet dotted-quads (3-octet semver can't match). Scan code and
# config only (skip *.md/*.log - documentation legitimately embeds well-known
# public IPs like well-known DNS resolvers and cloud egress ranges, which are
# not secrets). Allow loopback, RFC1918, link-local, RFC5737 TEST-NET, broadcast
# (0/255). Any remaining dotted-quad in code/config is a public host = fail.
$ipRe = '\b((25[0-5]|2[0-4][0-9]|1?[0-9][0-9]?)\.){3}(25[0-5]|2[0-4][0-9]|1?[0-9][0-9]?)\b'
$privateIpRe = '(^|[^0-9])(127|0|10|192\.168|172\.(1[6-9]|2[0-9]|3[01])|169\.254|192\.0\.2|198\.51\.100|203\.0\.113|255)\.'
$ipHits = @()
foreach ($f in @(Get-TrackedAll)) {
    if ($f -match '\.(md|log)$') { continue }
    $c = Read-FileOrNull $f
    if ($null -eq $c) { continue }
    foreach ($m in [regex]::Matches($c, $ipRe)) {
        if ($m.Value -match $privateIpRe) { continue }
        $ipHits += "$f : $($m.Value)"
    }
}
if ($ipHits.Count -gt 0) {
    foreach ($h in $ipHits) { Write-Host $h -ForegroundColor Yellow }
    Err "PD-2: public IP literal in tracked code/config (use a placeholder / env var)"
}

# ---- PD-1: live mode must never be committed ---------------------------------
$liveRe = '(?m)^\s*mode\s*=\s*"?live"?\s*(#.*)?$'
$liveHits = @()
foreach ($f in @(Get-TrackedPat @('*.toml', '*.yaml', '*.yml'))) {
    $c = Read-FileOrNull $f
    if ($null -eq $c) { continue }
    foreach ($m in [regex]::Matches($c, $liveRe)) { $liveHits += "$f : $($m.Value)" }
    # bash grep is case-sensitive (no -i) on this check; [regex]::Matches is
    # case-sensitive by default - parity holds.
}
if ($liveHits.Count -gt 0) {
    foreach ($h in $liveHits) { Write-Host $h -ForegroundColor Yellow }
    Err "PD-1: mode = live found in tracked config"
}

# ---- PD-3 / CONV-5: no wall clock on decision paths --------------------------
# Same allowlist as the bash version: tests/benches, the ONE sanctioned
# wall-clock reader core/src/wall_clock.rs, and the live historical-download
# edge storage/src/historical_download.rs. Match actual calls (`::now(`) so
# doc-comment mentions don't false-positive. (A-10, audit 2026-09-02: the
# needless storage/src/audit.rs exemption was removed — the file has no clock
# call and must not silently gain one.)
$clockRe = '(SystemTime|Instant|Utc|Local)::now\('
$clockHits = @()
foreach ($f in @(Get-TrackedPat @('core/**/*.rs', 'features/**/*.rs', 'strategies/**/*.rs', 'sim/**/*.rs', 'risk/**/*.rs', 'funnel/**/*.rs', 'storage/**/*.rs'))) {
    if ($f -match '(^|/)(tests|benches)/') { continue }
    if ($f -eq 'core/src/wall_clock.rs' -or $f -eq 'storage/src/historical_download.rs') { continue }
    $full = Join-Path $root $f
    if (-not (Test-Path -LiteralPath $full)) { continue }
    $m = Select-String -LiteralPath $full -Pattern $clockRe -AllMatches -CaseSensitive
    foreach ($x in $m) { $clockHits += "$($x.Path):$($x.LineNumber)" }
}
if ($clockHits.Count -gt 0) {
    foreach ($h in $clockHits) { Write-Host $h -ForegroundColor Yellow }
    Err "PD-3/CONV-5: wall-clock call on a decision-path crate (inject core::Clock)"
}

# ---- PD-4 / CONV-3: strategies and features stay offline ---------------------
$netRe = '^\s*(reqwest|hyper|tokio-tungstenite|tungstenite|ureq|surf|awc|isahc)\b'
foreach ($crate in @('strategies', 'features')) {
    $mf = Join-Path $root "$crate/Cargo.toml"
    if (Test-Path $mf) {
        $hit = Select-String -LiteralPath $mf -Pattern $netRe -CaseSensitive
        if ($hit) {
            foreach ($x in $hit) { Write-Host $x.Line -ForegroundColor Yellow }
            Err "PD-4/CONV-3: network dependency in $crate/Cargo.toml"
        }
    }
}

# ---- W-7: spec index consistency ----------------------------------------------
# Every specs/NNN-*.md appears in the README status table, and vice versa.
$readme = Join-Path $root 'specs/README.md'
if (Test-Path $readme) {
    $readmeText = Get-Content -LiteralPath $readme -Raw
    foreach ($f in @(Get-ChildItem (Join-Path $root 'specs') -Filter '[0-9][0-9][0-9]-*.md' -File)) {
        if ($readmeText -cnotmatch [regex]::Escape($f.Name)) {
            Err "W-7: $($f.Name) missing from specs/README.md status table"
        }
    }
    foreach ($m in [regex]::Matches($readmeText, '\]\([0-9]{3}-[a-z-]+\.md\)')) {
        $ref = $m.Value.TrimStart('](').TrimEnd(')')
        if (-not (Test-Path (Join-Path $root "specs/$ref"))) {
            Err "W-7: specs/README.md references missing spec $ref"
        }
    }
}

# ---- CONV-21/W-2: implemented specs must have ID-bearing tests ---------------
# For each spec marked 'implemented' in the status table, every requirement
# ID defined in it must appear (lowercased, underscored) in at least one test
# name. Test files are read once into memory for speed (one regex per id).
if (Test-Path $readme) {
    $implSpecs = @()
    foreach ($line in @(Get-Content -LiteralPath $readme)) {
        if ($line -match '^\|\s*[0-9]{3}\s*\|' -and $line -match 'implemented') {
            $mm = [regex]::Match($line, '\(([0-9]{3}-[a-z-]+\.md)\)')
            if ($mm.Success) { $implSpecs += $mm.Groups[1].Value }
        }
    }
    $testBlob = ''
    foreach ($tf in @(Get-TrackedPat @('*.rs', '*.py'))) {
        $c = Read-FileOrNull $tf
        if ($null -ne $c) { $testBlob += "`n$c" }
    }
    foreach ($spec in $implSpecs) {
        $specText = Read-FileOrNull "specs/$spec"
        if ($null -eq $specText) { continue }
        $ids = @([regex]::Matches($specText, '\*\*[A-Z]{3,4}-[0-9]+\*\*') | ForEach-Object { $_.Value.Trim('*') } | Sort-Object -Unique)
        foreach ($id in $ids) {
            $needle = $id.ToLower().Replace('-', '_')
            if ($testBlob -cnotmatch "fn ${needle}[a-z0-9_]*|def (test_)?${needle}[a-z0-9_]*") {
                Err "CONV-21: $spec is 'implemented' but no test name embeds ${id} (expected fn/def ${needle}_*)"
            }
        }
    }
}

# ---- OPS-4: every registered alert has a runbook -----------------------------
# Each alert!("id", SEV) row in the ops registry MUST have ops/runbooks/id.md.
$registry = Join-Path $root 'ops/src/registry.rs'
if (Test-Path $registry) {
    $ids = @([regex]::Matches((Get-Content -LiteralPath $registry -Raw), 'alert!\("[a-z0-9-]+"') `
        | ForEach-Object { $_.Value -replace 'alert!\("', '' -replace '"$', '' } | Sort-Object -Unique)
    foreach ($id in $ids) {
        if (-not (Test-Path (Join-Path $root "ops/runbooks/${id}.md"))) {
            Err "OPS-4: alert '$id' has no ops/runbooks/${id}.md"
        }
    }
}

# ---- CONV-3: one crate per top-level dir (workspace members are real dirs) ----
$rootManifest = Join-Path $root 'Cargo.toml'
if (Test-Path $rootManifest) {
    $text = Get-Content -LiteralPath $rootManifest -Raw
    $memberLine = [regex]::Match($text, '(?m)^\s*members\s*=.*$')
    if ($memberLine.Success) {
        foreach ($mm in [regex]::Matches($memberLine.Value, '"[a-z0-9_-]+"')) {
            $member = $mm.Value.Trim('"')
            if (-not (Test-Path (Join-Path $root "$member/Cargo.toml"))) {
                Err "CONV-3: workspace member '$member' has no $member/Cargo.toml"
            }
            if ($member -match '/') {
                Err "CONV-3: workspace member '$member' is not a top-level directory"
            }
        }
    }
}

# ---- ALP-2: new strategy crate must have registry row + hypothesis.md ------
# Every strategies/src/*.rs file must have a matching strategies/{id}/hypothesis.md
# and a row in research/registry.jsonl (spec 053 ALP-2).
$strategiesDir = Join-Path $root 'strategies'
$registryPath = Join-Path $root 'research' 'registry.jsonl'
if (Test-Path $strategiesDir) {
    foreach ($f in @(Get-ChildItem -Path (Join-Path $strategiesDir 'src') -Filter '*.rs' -File -ErrorAction SilentlyContinue)) {
        $id = $f.BaseName
        # Skip lib.rs (the trait module) and mod.rs
        if ($id -in @('lib', 'mod')) { continue }
        $hypPath = Join-Path $strategiesDir $id 'hypothesis.md'
        if (-not (Test-Path $hypPath)) {
            Err "ALP-2: strategy crate $id has no hypothesis.md"
        }
    }
}

# ---- Skill frontmatter sanity --------------------------------------------------
foreach ($f in @(Get-ChildItem (Join-Path $root '.claude\skills') -Recurse -Filter 'SKILL.md' -File -ErrorAction SilentlyContinue)) {
    $head = Get-Content -LiteralPath $f.FullName -TotalCount 1
    if ($head -ne '---') { Err "skill $($f.FullName) missing YAML frontmatter" }
    $c = Get-Content -LiteralPath $f.FullName -Raw
    if ($c -cnotmatch '(?m)^name:' -or $c -cnotmatch '(?m)^description:') {
        Err "skill $($f.FullName) missing name/description"
    }
}

Pop-Location
if ($script:fail -ne 0) {
    Write-Host ""
    Write-Host "Guardrails failed. Rules live in CLAUDE.md; do not weaken this script to pass (PD-5)." -ForegroundColor Red
    Exit 1
}
Write-Host "guardrails: all checks passed"
Exit 0
