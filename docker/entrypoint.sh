#!/bin/sh
# Starts sshd (for the SSH tunnel test, which tunnels through dev@localhost),
# then runs the command as `dev` inside a D-Bus session with an unlocked
# GNOME Keyring, so the Secret Service keychain works as on a desktop.
set -eu

if [ ! -f /home/dev/.ssh/id_ed25519 ]; then
    runuser -u dev -- sh -c '
        mkdir -p ~/.ssh && chmod 700 ~/.ssh
        ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/id_ed25519
        cp ~/.ssh/id_ed25519.pub ~/.ssh/authorized_keys
    '
fi
/usr/sbin/sshd
runuser -u dev -- sh -c 'ssh-keyscan -q localhost > ~/.ssh/known_hosts 2>/dev/null'

exec runuser -u dev -- dbus-run-session -- sh -c '
    printf podelis | gnome-keyring-daemon --unlock --components=secrets > /dev/null
    exec "$@"
' sh "$@"
