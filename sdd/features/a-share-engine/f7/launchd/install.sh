#!/bin/bash
# Installs the 2026-09-10 one-shot launchd jobs for the R2 follow-up:
# six raw captures at the session windows, a fresh Claude session at 09:32
# (opening interim) and one at 15:05 (session result). `uninstall` removes them.
# Jobs fire on Month=9 Day=10 only; run `install.sh uninstall` after the day.
set -eu
W=/Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility
D=$W/sdd/features/a-share-engine/f7
CAP=$D/capture-intraday.sh
LA=$HOME/Library/LaunchAgents
PREFIX=com.marketrig.a-share-feasibility
UID_=$(id -u)

if [ "${1:-install}" = uninstall ]; then
  for p in "$LA"/$PREFIX.*.plist; do
    [ -e "$p" ] || continue
    launchctl bootout "gui/$UID_" "$p" 2>/dev/null || true
    rm -f "$p"
    echo "removed $p"
  done
  exit 0
fi

mkdir -p "$D/intraday"
plist() { # name hour minute program args...
  local name=$1 h=$2 m=$3; shift 3
  local args=""; for a in "$@"; do args+="    <string>$a</string>"$'\n'; done
  cat > "$LA/$PREFIX.$name.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$PREFIX.$name</string>
  <key>ProgramArguments</key><array>
$args  </array>
  <key>WorkingDirectory</key><string>$W</string>
  <key>EnvironmentVariables</key><dict>
    <key>TZ</key><string>Asia/Shanghai</string>
    <key>PATH</key><string>/Users/xyril/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>StartCalendarInterval</key><dict>
    <key>Month</key><integer>9</integer><key>Day</key><integer>10</integer>
    <key>Hour</key><integer>$h</integer><key>Minute</key><integer>$m</integer>
  </dict>
  <key>StandardOutPath</key><string>$D/intraday/launchd-$name.log</string>
  <key>StandardErrorPath</key><string>$D/intraday/launchd-$name.log</string>
</dict></plist>
EOF
  plutil -lint "$LA/$PREFIX.$name.plist" >/dev/null
  launchctl bootout "gui/$UID_" "$LA/$PREFIX.$name.plist" 2>/dev/null || true
  launchctl bootstrap "gui/$UID_" "$LA/$PREFIX.$name.plist"
  echo "installed $PREFIX.$name at $h:$m Asia/Shanghai"
}

plist open-0931   9 31 /bin/bash "$CAP" open-0931
plist claude-open 9 32 /bin/bash "$D/launchd/run-claude.sh" "$D/launchd/prompt-open.md"
plist open-0945   9 45 /bin/bash "$CAP" open-0945
plist lunch-1259 12 59 /bin/bash "$CAP" lunch-1259
plist lunch-1301 13  1 /bin/bash "$CAP" lunch-1301
plist pm-1430    14 30 /bin/bash "$CAP" pm-1430
plist close-1457 14 57 /bin/bash "$CAP" close-1457
plist claude-close 15 5 /bin/bash "$D/launchd/run-claude.sh" "$D/launchd/prompt-close.md"

launchctl list | grep "$PREFIX" || true
