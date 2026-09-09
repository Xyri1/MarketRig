#!/usr/bin/env bash
# F7/R2 — one bounded HiThink sample set for one market window.
#
#   ./capture-intraday.sh <label>          # label: open-0931 open-0945 lunch-1259
#                                          #        lunch-1301 pm-1430 close-1457
#
# Read-only GETs against the same base URL and the same `X-api-key` header
# `crates/marketrigd/src/hithink.rs` uses (BASE_URL:35, attempt():464). One
# invocation issues at most 14 requests: 3 x (batched snapshot + 3 daily-bar
# reads) ~20 s apart, then one calendar and one auction read. It stops at the
# first HTTP 429 or envelope `code: 429` and still writes what it has.
#
# Output: f7/intraday/<label>-<YYYYMMDDTHHMMSS+0800>.json, key-free by
# construction (the key is only ever a header) and verified key-free before the
# file is kept. Safe under cron: absolute paths, no tty, one file per run.
set -euo pipefail
export TZ=Asia/Shanghai

BASE_URL="https://fuyao.aicubes.cn"
SNAPSHOT_CODES="600519.SH,601318.SH,000001.SZ,000858.SZ,300750.SZ"   # crates/marketrigd/src/catalog.rs:106-110
BAR_CODES="600519.SH 000001.SZ 300750.SZ"                            # historical is one thscode per request
ROUNDS=3
INTERVAL_S="${CAPTURE_INTERVAL_S:-20}"
ENV_FILE="${MARKETRIG_ENV_FILE:-/Users/xyril/Projects/MarketRig/.env}"

label="${1:-}"
case "$label" in
  '' | *[!a-z0-9-]* ) echo "usage: $0 <label>  (lowercase, digits, dashes)" >&2; exit 2 ;;
esac

here="$(cd "$(dirname "$0")" && pwd)"
out_dir="$here/intraday"
mkdir -p "$out_dir"
stamp="$(date +%Y%m%dT%H%M%S%z)"
out="$out_dir/$label-$stamp.json"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export HITHINK_API_KEY="$(grep '^HITHINK_API_KEY=' "$ENV_FILE" | cut -d= -f2-)"
[ -n "${HITHINK_API_KEY}" ] || { echo "no HITHINK_API_KEY in $ENV_FILE" >&2; exit 2; }

have_jq=0; command -v jq >/dev/null 2>&1 && have_jq=1
command -v python3 >/dev/null 2>&1 || [ "$have_jq" = 1 ] || { echo "neither jq nor python3" >&2; exit 2; }

now() { date +%Y-%m-%dT%H:%M:%S%z; }

# extract <kind> <body-file> -> one compact JSON object on stdout. jq when it is
# installed, otherwise python3 -c for parsing only. Both keep the same fields.
extract() {
  local kind="$1" body="$2"
  if [ "$have_jq" = 1 ]; then
    case "$kind" in
      snapshot) jq -c '{envelope_timestamp: .data.timestamp,
                        item: [.data.item[]? | {thscode, last_price, volume, turnover,
                                                prev_price, price_change}]}' "$body" ;;
      historical) jq -c '{envelope_timestamp: .data.timestamp,
                          bars: [.data.item[]?] | sort_by(.date_ms) | .[-3:]
                                | map({date_ms, date: (((.date_ms/1000)+28800) | strftime("%Y-%m-%d")),
                                       close_price, volume})}' "$body" ;;
      calendar) jq -c '{envelope_timestamp: .data.timestamp,
                        count: ([.data.item[]?] | length),
                        max_date: ([.data.item[]?.date] | max)}' "$body" ;;
      auction) jq -c '{envelope_timestamp: .data.timestamp, auction_phase: .data.auction_phase,
                       data_status: .data.data_status,
                       item: [.data.item[]? | {thscode, pre_close_price, auction_price, last_price}]}' "$body" ;;
    esac 2>/dev/null || echo 'null'
  else
    python3 -c '
import json,sys,time
kind,path=sys.argv[1],sys.argv[2]
try: d=json.load(open(path)).get("data") or {}
except Exception: print("null"); raise SystemExit
it=d.get("item") or []
def day(ms): return time.strftime("%Y-%m-%d", time.gmtime(ms/1000+28800))
if kind=="snapshot":
    o={"envelope_timestamp":d.get("timestamp"),
       "item":[{k:x.get(k) for k in ("thscode","last_price","volume","turnover","prev_price","price_change")} for x in it]}
elif kind=="historical":
    b=sorted(it,key=lambda x:x.get("date_ms") or 0)[-3:]
    o={"envelope_timestamp":d.get("timestamp"),
       "bars":[{"date_ms":x.get("date_ms"),"date":day(x.get("date_ms") or 0),
                "close_price":x.get("close_price"),"volume":x.get("volume")} for x in b]}
elif kind=="calendar":
    o={"envelope_timestamp":d.get("timestamp"),"count":len(it),
       "max_date":max([x.get("date") for x in it if x.get("date")] or [None])}
else:
    o={"envelope_timestamp":d.get("timestamp"),"auction_phase":d.get("auction_phase"),
       "data_status":d.get("data_status"),
       "item":[{k:x.get(k) for k in ("thscode","pre_close_price","auction_price","last_price")} for x in it]}
print(json.dumps(o,ensure_ascii=False,separators=(",",":")))' "$kind" "$body" 2>/dev/null || echo 'null'
  fi
}

envelope_code() {
  if [ "$have_jq" = 1 ]; then jq -c '.code // null' "$1" 2>/dev/null || echo null
  else python3 -c 'import json,sys;print(json.dumps(json.load(open(sys.argv[1])).get("code")))' "$1" 2>/dev/null || echo null
  fi
}

lines="$tmp/requests.jsonl"; : > "$lines"
stopped="null"
requests=0

# get <kind> <path> <query>; appends one record, returns 1 once rate limited.
get() {
  local kind="$1" path="$2" query="$3"
  local body="$tmp/body.json" started ended status code ex
  started="$(now)"
  status="$(curl -sS --max-time 30 -o "$body" -w '%{http_code}' \
              -H "X-api-key: $HITHINK_API_KEY" "$BASE_URL/api/$path?$query" || echo 000)"
  ended="$(now)"
  requests=$((requests + 1))
  code="$(envelope_code "$body")"
  ex="$(extract "$kind" "$body")"
  printf '{"kind":"%s","path":"/api/%s","query":"%s","started_at":"%s","ended_at":"%s","http_status":%s,"envelope_code":%s,"extract":%s}\n' \
    "$kind" "$path" "$query" "$started" "$ended" "$status" "$code" "$ex" >> "$lines"
  if [ "$status" = "429" ] || [ "$code" = "429" ]; then
    stopped="\"RATE_LIMITED http $status code $code\""
    return 1
  fi
  return 0
}

run_start="$(now)"
for round in $(seq 1 "$ROUNDS"); do
  [ "$round" -eq 1 ] || sleep "$INTERVAL_S"
  get snapshot "a-share/prices/snapshot" "thscodes=$SNAPSHOT_CODES" || break
  end_ms=$(( $(date +%s) * 1000 )); start_ms=$(( end_ms - 864000000 ))   # 10 days
  for code in $BAR_CODES; do
    get historical "a-share/prices/historical" \
        "thscode=$code&interval=1d&start=$start_ms&end=$end_ms&adjust=none" || break 2
  done
done
if [ "$stopped" = "null" ]; then
  get calendar "a-share/calendar/trading-days" "" || true
fi
if [ "$stopped" = "null" ]; then
  get auction "a-share/auction/snapshot" "thscodes=$SNAPSHOT_CODES&stage=final" || true
fi
run_end="$(now)"

note="session sample"
case "$label" in dryrun-*) note="after-hours dry run — NOT a session sample" ;; esac

{
  printf '{\n "label": "%s",\n "note": "%s",\n "base_url": "%s",\n' "$label" "$note" "$BASE_URL"
  printf ' "started_at": "%s",\n "ended_at": "%s",\n "requests_issued": %s,\n "stopped_early": %s,\n' \
         "$run_start" "$run_end" "$requests" "$stopped"
  printf ' "snapshot_codes": "%s",\n "bar_codes": "%s",\n "rounds": %s,\n "interval_s": %s,\n' \
         "$SNAPSHOT_CODES" "$BAR_CODES" "$ROUNDS" "$INTERVAL_S"
  printf ' "requests": [\n'
  sed '$!s/$/,/' "$lines"
  printf ' ]\n}\n'
} > "$tmp/out.json"

if [ "$have_jq" = 1 ]; then jq . "$tmp/out.json" > "$out"; else cp "$tmp/out.json" "$out"; fi

if grep -qF -- "$HITHINK_API_KEY" "$out"; then
  rm -f "$out"; echo "the api key reached the artifact; the sample was discarded" >&2; exit 4
fi
echo "$out"
