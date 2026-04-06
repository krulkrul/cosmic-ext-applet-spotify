#!/usr/bin/env bash
set -e
cargo build --release

# Restart the panel to load the new applet binary.
#
# Strategy: only kill a *manually-launched* panel (ppid != cosmic-session),
# then re-launch one ourselves.  This avoids incrementing cosmic-session's
# exponential restart backoff counter, which otherwise grows with every kill
# and eventually causes multi-minute waits.
#
# If only a session-managed panel is found (first run of a fresh session),
# we fall back to killing it and waiting for cosmic-session to restart it.
# That costs one backoff increment but avoids a duplicate panel.

SESSION_PID=$(pgrep -x cosmic-session | head -1)

kill_pid=""
for pid in $(pgrep -x cosmic-panel 2>/dev/null); do
    ppid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
    if [ "$ppid" != "$SESSION_PID" ]; then
        kill_pid="$pid"
        break
    fi
done

if [ -n "$kill_pid" ]; then
    # Kill the manually-launched panel and re-launch.
    kill -TERM "$kill_pid" 2>/dev/null || true
    sleep 2
    WAYLAND_DISPLAY=wayland-1 cosmic-panel &
else
    # Only a session-managed panel found — kill it and let cosmic-session restart it.
    # If the session is in backoff, we re-launch manually after a short wait.
    pkill -TERM -x cosmic-panel 2>/dev/null || true
    for i in $(seq 1 6); do
        sleep 2
        if pgrep -x cosmic-panel > /dev/null 2>&1; then
            break
        fi
    done
    if ! pgrep -x cosmic-panel > /dev/null 2>&1; then
        WAYLAND_DISPLAY=wayland-1 cosmic-panel &
    fi
fi

sleep 3
echo "Applet reloaded."
