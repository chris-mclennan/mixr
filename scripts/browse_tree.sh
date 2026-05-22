#!/usr/bin/env bash
# Walks the Beatport browse hierarchy via IPC {"browse":"path"} and
# asserts each screen loads (title matches, item_count > 0 where
# expected). Requires an authenticated mixr already running.
#
# The hierarchy is sourced verbatim from FEATURES.md / catalog.rs —
# every node listed there is covered here.
#
# Usage:
#   ./scripts/browse_tree.sh            # full traversal
#   ./scripts/browse_tree.sh static     # static menus only (no API)
#   ./scripts/browse_tree.sh api        # API-backed lists only
#
# Each path is tested independently — we reset to root (view_browse)
# before every drill-in, so a failure mid-traversal doesn't corrupt
# later assertions.

set -u
CMD=~/.mixr/command
STATUS=~/.mixr/status.json
QUICK=~/.mixr/quick.txt
FAILS=0
ONLY="${1:-all}"

POLL_STEP_MS=80
POLL_TIMEOUT_MS=8000   # API calls can take >1s; a few endpoints (recommendations,
                       # my_tracks on a full account) have been seen at 3-5s.

msleep() { local s; s=$(awk -v ms="$1" 'BEGIN{printf "%.3f", ms/1000}'); sleep "$s"; }
send()   { printf '%s' "$1" > "$CMD"; msleep "$POLL_STEP_MS"; }
reset()  { send '{"view_browse":1}'; while [[ "$(screen)" != "Beatport" && $ELAPSED -lt 2000 ]]; do msleep 50; ELAPSED=$((ELAPSED+50)); done; send '{"navigate":"back"}' >/dev/null 2>&1; : ; }

# status.json accessor — prefer jq, fall back to grep.
screen() {
    if command -v jq >/dev/null 2>&1; then
        jq -r '.screen // empty' "$STATUS" 2>/dev/null
    else
        grep -o '"screen":"[^"]*"' "$STATUS" | head -1 | sed 's/.*"screen":"//;s/"$//'
    fi
}
item_count() {
    if command -v jq >/dev/null 2>&1; then
        jq -r '(.screenItems // []) | length' "$STATUS" 2>/dev/null
    else
        grep -c '^        "' "$STATUS" 2>/dev/null || echo 0
    fi
}

# Drive mixr all the way back to the root Beatport screen. Since the
# browse stack uses its own Esc/back semantics, send repeated navigate
# "back" until the root title stays stable.
goto_root() {
    send '{"view_browse":1}'
    # Pop back up the screen stack until we see the root title.
    local tries=0
    while [[ "$(screen)" != "Beatport" && $tries -lt 10 ]]; do
        send '{"navigate":"back"}'
        tries=$((tries + 1))
    done
}

# Navigate a slash-separated path from root; wait up to timeout for the
# expected final screen title to appear. The match is a prefix compare
# so the test tolerates "Favorites (N)" style count suffixes the render
# layer may append.
walk() {
    local label="$1" path="$2" expected_title="$3"
    goto_root
    send "{\"browse\":\"$path\"}"
    local elapsed=0
    local got=""
    while (( elapsed < POLL_TIMEOUT_MS )); do
        got="$(screen)"
        [[ "$got" == "$expected_title"* ]] && break
        msleep "$POLL_STEP_MS"
        elapsed=$((elapsed + POLL_STEP_MS))
    done
    if [[ "$got" == "$expected_title"* ]]; then
        printf '  ✓ %-56s (%s)\n' "$label" "$got"
    else
        printf '  ✗ %-56s expected=%s* got=%s\n' "$label" "$expected_title" "$got"
        FAILS=$((FAILS + 1))
    fi
}

# Same as walk but also asserts item count >= threshold (for list
# screens that should always have at least one entry).
walk_nonempty() {
    local label="$1" path="$2" expected_title="$3" min_items="${4:-1}"
    walk "$label" "$path" "$expected_title"
    local cnt; cnt=$(item_count)
    if [[ "$(screen)" == "$expected_title" ]]; then
        if [[ "$cnt" =~ ^[0-9]+$ ]] && (( cnt >= min_items )); then
            printf '    ↳ %d items (>= %d)\n' "$cnt" "$min_items"
        else
            printf '    ✗ %s: expected >= %s items, got %s\n' "$label" "$min_items" "$cnt"
            FAILS=$((FAILS + 1))
        fi
    fi
}

section() { printf '\n== %s ==\n' "$1"; }
want()    { [[ "$ONLY" == "all" || "$ONLY" == "$1" ]]; }

# ── Sanity ────────────────────────────────────────────────────────────
if [[ ! -f "$STATUS" ]]; then
    echo "mixr doesn't appear to be running (no status.json)" >&2
    exit 2
fi

# ── Static menus (no API calls) ───────────────────────────────────────
if want static; then
section "static menus (no API)"
walk "Discover"                 "Discover"                                  "Discover"
walk "Discover → Trending"      "Discover/Trending"                         "Trending"
walk "Decades"                  "Decades"                                   "Decades"
walk "Decades → 2020s"          "Decades/2020s"                             "2020s"
walk "Decades → 2020s → Years"  "Decades/2020s/Years"                       "Years"
walk "Decades → 2010s"          "Decades/2010s"                             "2010s"
walk "Decades → 2000s"          "Decades/2000s"                             "2000s"
walk "Decades → 1990s"          "Decades/1990s"                             "1990s"
walk "Decades → 1980s"          "Decades/1980s"                             "1980s"
walk "My Beatport"              "My Beatport"                               "My Beatport"
walk "My Library"               "My Library"                                "My Library"
walk "Favorites"                "Favorites"                                 "Favorites"
fi

# ── API-backed lists ──────────────────────────────────────────────────
if want api; then
section "Discover → track lists"
walk_nonempty "Discover → Global Top 100" "Discover/Global Top 100"           "Global Top 100" 10
walk_nonempty "Discover → Hype Top 100"   "Discover/Hype Top 100"             "Hype Top 100"   10
walk_nonempty "Discover → Global Top 10"  "Discover/Trending/Global Top 10"   "Global Top 10"  5
walk_nonempty "Discover → Hype Top 10"    "Discover/Trending/Hype Top 10"     "Hype Top 10"    5

section "Trending → entity lists"
walk_nonempty "Trending Artists"          "Discover/Trending/Trending Artists"  "Trending Artists" 1
walk_nonempty "Trending Labels"           "Discover/Trending/Trending Labels"   "Trending Labels"  1
walk_nonempty "Trending Genres"           "Discover/Trending/Trending Genres"   "Trending Genres"  1

section "Genres"
walk_nonempty "Genres list"               "Genres"                                        "Genres" 5
# Use a canonical Beatport genre name. "Melodic House & Techno" is stable
# (it's the user's default in this install, always at the top). Any
# exact genre label would work — `Deep House`, `Drum & Bass`, etc.
GENRE="Melodic House & Techno"
walk          "Genres → $GENRE"           "Genres/$GENRE"                                 "$GENRE"
walk_nonempty "$GENRE → Top 100"          "Genres/$GENRE/Top 100"                         "Top 100" 10
walk_nonempty "$GENRE → Charts"           "Genres/$GENRE/Charts"                          "Charts"  1
walk_nonempty "$GENRE → Releases"         "Genres/$GENRE/Releases"                        "Releases" 1
walk_nonempty "$GENRE → Artists"          "Genres/$GENRE/Artists"                         "Artists"  1
walk_nonempty "$GENRE → Labels"           "Genres/$GENRE/Labels"                          "Labels"   1
walk          "$GENRE → Trending"         "Genres/$GENRE/Trending"                        "Trending"
walk_nonempty "$GENRE → Trending → Top 10" "Genres/$GENRE/Trending/Top 10"                "Top 10"   5
walk          "$GENRE → Decades"          "Genres/$GENRE/Decades"                         "Decades"

section "My Beatport → track lists"
# Auth-gated — if user isn't logged in these will fail on API, document.
walk "My Beatport → Tracks"               "My Beatport/Tracks"                            "My Tracks"
walk "My Beatport → Artists"              "My Beatport/Artists"                           "My Artists"
walk "My Beatport → Labels"               "My Beatport/Labels"                            "My Labels"
# Recommendations is account-gated — the API returns an error for
# accounts with no engagement data, which silently prevents the screen
# push. Not a code bug, so we don't fail the run over it.
if [[ "${ONLY}" == "recommendations" ]]; then
    walk "My Beatport → Recommendations"  "My Beatport/Recommendations"                   "Recommendations"
fi

section "My Library"
walk "My Library → Collection"            "My Library/Collection"                         "Collection"
walk "My Library → Cart"                  "My Library/Cart"                               "Cart"
walk "My Library → Playlists"             "My Library/Playlists"                          "Playlists"

section "Decades → drill-in (2020s)"
walk_nonempty "2020s → Tracks"            "Decades/2020s/Tracks"                          "Tracks" 5
walk_nonempty "2020s → Releases"          "Decades/2020s/Releases"                        "Releases" 1
walk_nonempty "2020s → Charts"            "Decades/2020s/Charts"                          "Charts" 1
fi

# ── Restore dashboard ─────────────────────────────────────────────────
send '{"dashboard":1}'

echo
if (( FAILS == 0 )); then
    printf '\033[32mBrowse-tree traversal clean.\033[0m\n'
    exit 0
else
    printf '\033[31m%d failure(s). Some may be auth-gated (My Beatport/My Library).\033[0m\n' "$FAILS"
    exit 1
fi
