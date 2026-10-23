# syntax=docker/dockerfile:1
#
# mp3blaster-rs on Ubuntu.
#
#   docker build -t mp3blaster .                      build the player image
#   docker build --target test .                      fmt, clippy and the test suite on Linux
#   docker run -it --rm mp3blaster                    play the bundled samples
#   docker run -it --rm -v "D:\Music:/music:ro" mp3blaster      play your own music
#
# There is no sound card in a container: audio goes to a silent PulseAudio sink that runs in
# real time, so playback, the spectrum and the visualizers behave as on a real device. Set
# PULSE_SERVER to play through a PulseAudio server outside the container instead.

ARG UBUNTU=24.04

FROM ubuntu:${UBUNTU} AS toolchain
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential pkg-config libasound2-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
        --component clippy,rustfmt
WORKDIR /src

# The same checks as on Windows. None of the tests need a sound card.
FROM toolchain AS test
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=mp3blaster-test-target \
    cargo fmt --check \
    && cargo clippy --locked --all-targets -- -D warnings \
    && cargo test --locked

FROM toolchain AS build
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=mp3blaster-release-target \
    cargo build --locked --release \
    && cp target/release/mp3blaster-rs /usr/local/bin/

FROM ubuntu:${UBUNTU} AS player
ENV DEBIAN_FRONTEND=noninteractive
# ALSA with its PulseAudio plugin, and PulseAudio for the silent sink.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libasound2t64 libasound2-plugins pulseaudio \
    && rm -rf /var/lib/apt/lists/*
COPY docker/asound.conf /etc/asound.conf
COPY --from=build /usr/local/bin/mp3blaster-rs /usr/local/bin/mp3blaster-rs
COPY docker/entrypoint.sh /usr/local/bin/mp3blaster-entrypoint
RUN sed -i 's/\r$//' /usr/local/bin/mp3blaster-entrypoint \
    && chmod 755 /usr/local/bin/mp3blaster-entrypoint \
    && useradd --create-home --shell /bin/bash player
# The test tones as sample music; mount a folder over /music to play your own.
COPY tests/fixtures/*.flac tests/fixtures/*.mp3 tests/fixtures/*.ogg tests/fixtures/*.m4a /music/samples/
COPY --chown=player:player docker/config.toml /home/player/.config/mp3blaster-rs/config.toml

USER player
WORKDIR /home/player
ENV LANG=C.UTF-8 \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    XDG_RUNTIME_DIR=/tmp/runtime-player
ENTRYPOINT ["mp3blaster-entrypoint"]
CMD ["/music"]
