# Selects one attended experiment cell on Windows and seeds E6's environment
# (EXPERIMENT.md §1, §2, §7).
#
#   . crates\marketrig-acceptance\experiment-env.ps1 codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# The PowerShell twin of experiment-env.sh. E1–E4 need only the cell; E6 also
# needs a Python that reports exactly 3.12, a Node 22 or newer, the locked
# wheel directory `node scripts\openviking-wheels.mjs` produced, and one
# OpenAI-compatible provider. Anything missing leaves E6's variables unset,
# which skips E6 with evidence and still runs the rest of the cell. The key is
# taken from MARKETRIG_EXPERIMENT_MEMORY_API_KEY if already set, else read
# silently from the prompt; it is never echoed or written anywhere. Must be
# dot-sourced, not run, so the variables land in the calling shell.

param([string]$Cell = "codex")

if ($MyInvocation.InvocationName -ne ".") {
    Write-Error "dot-source this file (`. $($MyInvocation.MyCommand.Path) codex|claude`); do not run it"
    return
}
if ($Cell -notin @("codex", "claude")) {
    Write-Error "usage: . experiment-env.ps1 codex|claude"
    return
}

$env:MARKETRIG_EXPERIMENT = $Cell
Remove-Item Env:MARKETRIG_ACCEPTANCE_OUT -ErrorAction SilentlyContinue
Write-Host "cell=$env:MARKETRIG_EXPERIMENT"

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$python = $env:MARKETRIG_EXPERIMENT_PYTHON
if (-not $python) { $python = (Get-Command python3.12 -ErrorAction SilentlyContinue).Source }
if (-not $python) { $python = & py -3.12 -c "import sys;print(sys.executable)" 2>$null }
$node = $env:MARKETRIG_EXPERIMENT_NODE
if (-not $node) { $node = (Get-Command node -ErrorAction SilentlyContinue).Source }
$wheels = $env:MARKETRIG_EXPERIMENT_WHEELS
if (-not $wheels) { $wheels = Join-Path $root "openviking-wheels\windows-x64" }

# The same two probes the daemon runs (feature SPEC §1.2), so a wrong
# interpreter costs a line here instead of an attended hour.
$pythonMinor = if ($python) { & $python -c "import sys;print('%d.%d'%sys.version_info[:2])" 2>$null }
$nodeVersion = if ($node) { & $node --version 2>$null }
$ready = ($pythonMinor -eq "3.12") -and ("$nodeVersion" -match '^v(\d+)') -and ([int]$Matches[1] -ge 22) -and (Test-Path $wheels)

if ($ready -and -not $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY) {
    $secure = Read-Host -AsSecureString "provider API key for E6 (not echoed; empty skips E6)"
    $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY = [System.Net.NetworkCredential]::new("", $secure).Password
}
if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY) { $ready = $false }

if ($ready) {
    $env:MARKETRIG_EXPERIMENT_PYTHON = $python
    $env:MARKETRIG_EXPERIMENT_NODE = $node
    $env:MARKETRIG_EXPERIMENT_WHEELS = $wheels
    if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL) { $env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL = "https://openrouter.ai/api/v1" }
    if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL) { $env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL = "z-ai/glm-5.3-flash" }
    if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL) { $env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL = "openai/text-embedding-3-small" }
    Write-Host "E6: python=$env:MARKETRIG_EXPERIMENT_PYTHON node=$env:MARKETRIG_EXPERIMENT_NODE wheels=$env:MARKETRIG_EXPERIMENT_WHEELS"
    Write-Host "E6: base_url=$env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL llm=$env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL embedding=$env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL key=set"
}
else {
    Remove-Item Env:MARKETRIG_EXPERIMENT_PYTHON, Env:MARKETRIG_EXPERIMENT_NODE, Env:MARKETRIG_EXPERIMENT_WHEELS -ErrorAction SilentlyContinue
    $key = if ($env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY) { "set" } else { "unset" }
    Write-Host "E6 will skip: python=$python ($pythonMinor) node=$node ($nodeVersion) wheels=$wheels key=$key"
}
