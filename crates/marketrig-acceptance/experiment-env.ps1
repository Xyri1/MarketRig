# Selects one attended experiment cell on Windows (EXPERIMENT.md §1, §2).
#
#   . crates\marketrig-acceptance\experiment-env.ps1 codex   # or claude
#   cargo test -p marketrig-acceptance --test experiment -- --nocapture
#
# E5's Hindsight and provider variables went with Hindsight; E6's arrive with
# slice 011's C58. Must be dot-sourced, not run, so the variables land in the
# calling shell.

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
