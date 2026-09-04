# Seeds one attended experiment cell's environment on Windows (EXPERIMENT.md §1, §7).
#
#   . crates\marketrig-acceptance\experiment-env.ps1 codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# The PowerShell twin of experiment-env.sh: builds the Hindsight launcher venv
# once under $env:HINDSIGHT_VENV (default ~\.hindsight), verifies its
# capability marker, and exports the five R4 variables. The key is taken from
# MARKETRIG_EXPERIMENT_MEMORY_API_KEY if already set, else read silently from
# the prompt; it is never echoed or written anywhere. Must be dot-sourced, not
# run, so the variables land in the calling shell.

param([string]$Cell = "codex")

if ($MyInvocation.InvocationName -ne ".") {
    Write-Error "dot-source this file (`. $($MyInvocation.MyCommand.Path) codex|claude`); do not run it"
    return
}
if ($Cell -notin @("codex", "claude")) {
    Write-Error "usage: . experiment-env.ps1 codex|claude"
    return
}

$venv = if ($env:HINDSIGHT_VENV) { $env:HINDSIGHT_VENV } else { Join-Path $HOME ".hindsight" }
$launcher = Join-Path $venv "Scripts\hindsight-api.exe"
if (-not (Test-Path $launcher)) {
    Write-Host "building $venv (hindsight-api-slim[embedded-db]==0.9.2)…"
    uv venv $venv --python 3.12; if ($LASTEXITCODE) { return }
    uv pip install --python (Join-Path $venv "Scripts\python.exe") "hindsight-api-slim[embedded-db]==0.9.2"; if ($LASTEXITCODE) { return }
}
if (-not ((& $launcher --help 2>$null) -match "HINDSIGHT_API_PORT")) {
    Write-Error "$launcher does not print the HINDSIGHT_API_PORT marker; discovery would refuse it"
    return
}

if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY) {
    $secure = Read-Host -AsSecureString "provider API key (not echoed)"
    $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY = [System.Net.NetworkCredential]::new("", $secure).Password
}
if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_API_KEY) { Write-Error "no key given"; return }

$env:MARKETRIG_EXPERIMENT = $Cell
$env:MARKETRIG_EXPERIMENT_HINDSIGHT = $launcher
if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL) { $env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL = "https://openrouter.ai/api/v1" }
if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL) { $env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL = "z-ai/glm-5.3-flash" }
if (-not $env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL) { $env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL = "openai/text-embedding-3-small" }
Remove-Item Env:MARKETRIG_ACCEPTANCE_OUT -ErrorAction SilentlyContinue

Write-Host "cell=$env:MARKETRIG_EXPERIMENT launcher=$env:MARKETRIG_EXPERIMENT_HINDSIGHT"
Write-Host "base_url=$env:MARKETRIG_EXPERIMENT_MEMORY_BASE_URL llm=$env:MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL embedding=$env:MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL key=set"
