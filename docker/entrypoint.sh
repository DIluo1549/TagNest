#!/bin/sh
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
UMASK="${UMASK:-002}"

umask "$UMASK"
mkdir -p /data

if [ "$(id -u)" = "0" ]; then
    # Match the host user so files written back to a NAS share are not owned
    # by root. This only changes the container's internal account.
    groupmod -o -g "$PGID" tagnest 2>/dev/null || true
    usermod -o -u "$PUID" tagnest 2>/dev/null || true
    chown -R "$PUID:$PGID" /data
    exec gosu "$PUID:$PGID" /usr/local/bin/tagnest "$@"
fi

exec /usr/local/bin/tagnest "$@"
