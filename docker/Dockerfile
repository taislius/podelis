# Debian with everything Podėlis needs to build, run and pass every test,
# including the opt-in SSH tunnel and Secret Service (keychain) tests.
# Used by docker/compose.yaml; see docs/development.md.
FROM debian:trixie-slim

ENV LANG=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TARGET_DIR=/target

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config ca-certificates curl git less \
        openssh-client openssh-server \
        dbus dbus-user-session gnome-keyring libsecret-tools \
        postgresql-client default-mysql-client sqlite3 \
        ncurses-term procps \
    && rm -rf /var/lib/apt/lists/*

# The toolchain pinned in rust-toolchain.toml, installed into the image.
COPY rust-toolchain.toml /tmp/toolchain/
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none \
    && cd /tmp/toolchain && rustup toolchain install \
    && rm -rf /tmp/toolchain

ARG CARGO_DENY=0.20.2
RUN arch="$(uname -m)" \
    && curl -sSfL "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY}/cargo-deny-${CARGO_DENY}-${arch}-unknown-linux-musl.tar.gz" \
       | tar -xz --strip-components=1 -C /usr/local/cargo/bin "cargo-deny-${CARGO_DENY}-${arch}-unknown-linux-musl/cargo-deny"

# A normal user: tests check file permissions, which root ignores.
RUN useradd -m -s /bin/bash dev \
    && mkdir -p /target /usr/local/cargo/registry /run/sshd \
    && chown -R dev /target /usr/local/cargo

COPY docker/entrypoint.sh /usr/local/bin/entrypoint
WORKDIR /podelis
ENTRYPOINT ["entrypoint"]
CMD ["bash"]
