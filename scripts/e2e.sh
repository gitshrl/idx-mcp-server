#!/usr/bin/env bash
# End-to-end smoke test against a running idx-mcp server over Streamable HTTP.
# Usage: BASE=http://127.0.0.1:8080 KEY=idx_... ./scripts/e2e.sh
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
KEY="${KEY:?set KEY to a valid API key}"
URL="$BASE/mcp"
HDR=(-H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
     -H "Accept: application/json, text/event-stream")

# Extract the JSON object from a response body that may be raw JSON or SSE
# (`data: {...}` lines).
extract() { grep -a '^data:' | sed 's/^data: //' | tail -1 || cat; }

pass=0; fail=0
check() { # name, jq-filter, body
  local name="$1" filter="$2" body="$3"
  local got; got=$(printf '%s' "$body" | extract)
  if printf '%s' "$got" | jq -e "$filter" >/dev/null 2>&1; then
    echo "PASS  $name"; pass=$((pass+1))
  else
    echo "FAIL  $name"; echo "      $got" | head -c 400; echo; fail=$((fail+1))
  fi
}

req() { # method, params-json  -> body on stdout, session header captured
  local method="$1" params="$2"
  curl -sS -N -D /tmp/e2e_headers "${HDR[@]}" \
    ${SESSION:+-H "Mcp-Session-Id: $SESSION"} \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}" \
    "$URL"
}

# 1. initialize -> capture session id
INIT_BODY=$(req initialize '{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}')
SESSION=$(grep -ai '^mcp-session-id:' /tmp/e2e_headers | sed 's/^[^:]*: //' | tr -d '\r' || true)
echo "session: ${SESSION:-<none>}"
check "initialize" '.result.serverInfo.name' "$INIT_BODY"

# notifications/initialized (no response expected)
curl -sS "${HDR[@]}" ${SESSION:+-H "Mcp-Session-Id: $SESSION"} \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' "$URL" >/dev/null || true

# 2. tools/list -> expect the 9 tools
LIST=$(req tools/list '{}')
check "tools/list has run_query"     '.result.tools[].name | select(.=="run_query")'     "$LIST"
check "tools/list has describe_schema" '.result.tools[].name | select(.=="describe_schema")' "$LIST"
check "tools/list has screen_stocks"  '.result.tools[].name | select(.=="screen_stocks")'  "$LIST"

call() { req tools/call "{\"name\":\"$1\",\"arguments\":$2}"; }

# 3. describe_schema returns relations
check "describe_schema" '.result.content[0].text | fromjson | map(.name) | index("returns")' \
  "$(call describe_schema '{}')"

# 4. run_query: cross-view analytical query (latest JOIN returns) returns rows
check "run_query rows" '.result.content[0].text | fromjson | .row_count > 0' \
  "$(call run_query '{"sql":"SELECT l.ticker, l.sector, r.ret_1y FROM latest l JOIN returns r USING(ticker) WHERE r.ret_1y IS NOT NULL ORDER BY r.ret_1y DESC LIMIT 5"}')"

# 5. run_query: forbidden query is rejected (isError / error)
check "run_query rejects read_parquet" '(.result.isError == true) or (.error != null)' \
  "$(call run_query '{"sql":"SELECT * FROM read_parquet('x')"}')"

# 6. screen_stocks: typed filter
check "screen_stocks" '.result.content[0].text | fromjson | type == "array"' \
  "$(call screen_stocks '{"filters":[{"field":"dividend_yield","op":">","value":2}],"limit":5}')"

# 7. a shortcut still works
check "get_prices" '.result.content[0].text | fromjson | type == "array"' \
  "$(call get_prices '{"ticker":"BBCA","limit":3}')"

echo "--- $pass passed, $fail failed ---"
[ "$fail" -eq 0 ]
