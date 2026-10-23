#!/bin/sh
# Starts the silent sound server the player plays into, then the player itself.
set -e
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
if [ -z "$PULSE_SERVER" ]; then
    # A null sink consumes audio in real time, like a sound card nobody listens to.
    pulseaudio --daemonize=yes --exit-idle-time=-1 -n \
        --load="module-native-protocol-unix" \
        --load="module-null-sink sink_name=silent sink_properties=device.description=Silent" \
        >/dev/null 2>&1 \
        || echo "mp3blaster: PulseAudio did not start, so there is no audio device" >&2
fi
exec mp3blaster-rs "$@"
