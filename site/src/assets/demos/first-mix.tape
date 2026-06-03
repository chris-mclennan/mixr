# mixr — first-mix demo recording
#
# Regenerate site/src/assets/demos/first-mix.gif with VHS
# (https://github.com/charmbracelet/vhs):
#
#     cargo build --release          # the tape runs ./target/release/mixr
#     vhs site/src/assets/demos/first-mix.tape
#     # Optional shrink afterwards:
#     gifsicle --optimize=3 --lossy=80 \
#       site/src/assets/demos/first-mix.gif \
#       -o site/src/assets/demos/first-mix.gif
#
# Notes:
#   * The tape sets HOME to an isolated config dir so it doesn't fight
#     the long-running mixr instance (e.g. one launched by tmnl) for
#     ~/.mixr/command and ~/.mixr/status.json. Bootstrap that dir once
#     before recording (the auth.json is required — VHS can't drive the
#     embedded OAuth WebView):
#
#       mkdir -p /tmp/mixr-vhs-home/.mixr
#       cp ~/.mixr/auth.json    /tmp/mixr-vhs-home/.mixr/
#       cp ~/.mixr/session.json /tmp/mixr-vhs-home/.mixr/ 2>/dev/null
#       cp ~/.mixr/config.json  /tmp/mixr-vhs-home/.mixr/
#
#   * Recording captures the terminal only; audio is irrelevant. The
#     engine still drives the crossfader/EQ/fader animations during a
#     transition so the GIF *looks* like a real mix.
#
#   * `--play "Genre"` is the most reliable launch path — it kicks off
#     a Beatport Top-100 fetch in the background and lands the user on
#     the dashboard. We then take a brief detour into the browser to
#     show the drill-in shape, return to the dashboard, queue, play,
#     and force a mix.

Output site/src/assets/demos/first-mix.gif

Set Shell       "bash"
Set FontFamily  "JetBrainsMono Nerd Font mnml"
Set FontSize    14
Set Width       1280
Set Height      720
Set Padding     20
Set TypingSpeed 80ms
Set Theme       "Catppuccin Mocha"

# ── setup (hidden) ──────────────────────────────────────────────────────
# Isolated HOME so the tape doesn't trip the user's real mixr
# session or get killed by another running instance.
Hide
Type "export HOME=/tmp/mixr-vhs-home"
Enter
Sleep 200ms
Type "clear"
Enter
Sleep 200ms
Show

# ── launch ──────────────────────────────────────────────────────────────
# `--play "Genre"` preloads the genre's Top-100 chart in the
# background and opens the dashboard. The chart loads onto the
# screen stack so a later `a` queues all 100 tracks. `--dashboard`
# (implied by --play) is the landing view.
Type "./target/release/mixr --play 'Melodic House & Techno'"
Enter
Sleep 5s

# ── open the browser to show what was loaded ──────────────────────────
# `--play` pushed the genre's Top-100 onto the screen stack but
# left the user on the dashboard. `b` twice expands the Browse
# panel into the full-screen browser so the tracklist is visible.
Type "b"
Sleep 600ms
Type "b"
Sleep 4s

# ── queue all 100 tracks ───────────────────────────────────────────────
# `a` queues every visible track. Watch the QUEUE counter in the
# bottom-left chip jump from 0 → 100. (`d` then returns to the
# dashboard.)
Type "a"
Sleep 2500ms
Type "d"
Sleep 1500ms

# ── playback starts automatically ──────────────────────────────────────
# The first queued track loads onto Deck A and starts playing on
# its own; the second preloads on Deck B as the incoming track.
# The waveform on Deck A scrolls, BPM lights up, and the MIX IN
# countdown ticks down on the controller readout. (Pressing `p`
# here would *pause* the running deck — only useful for stopping.)
Sleep 5s

# ── force the crossfade ────────────────────────────────────────────────
# `m` triggers the transition immediately rather than waiting for
# the track's natural mix-in point. The crossfader sweeps from A
# to B over the configured number of bars (16 by default); the
# channel faders + filter knobs animate as the transition rule
# plays out. We linger so the GIF captures the whole sweep.
Type "m"
Sleep 9s
