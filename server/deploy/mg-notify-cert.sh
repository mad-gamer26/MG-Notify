#!/bin/sh
# Certbot deploy hook: copies the TLS certificate where the MG Notify server
# (running as its own user) can read it for STARTTLS on inbound email. The
# server reads the files for each connection, so no restart is needed.
# Install as /etc/letsencrypt/renewal-hooks/deploy/mg-notify-cert.sh (chmod 755).
set -eu
CERT_NAME="${MG_NOTIFY_CERT_NAME:-mad-gamer.com}"
OWNER="${MG_NOTIFY_USER:-matthew}"
DST="${MG_NOTIFY_TLS_DIR:-/home/$OWNER/notify/tls}"
SRC="/etc/letsencrypt/live/$CERT_NAME"
# Certbot sets RENEWED_LINEAGE; ignore renewals of other certificates.
if [ -n "${RENEWED_LINEAGE:-}" ] && [ "$RENEWED_LINEAGE" != "$SRC" ]; then
    exit 0
fi
install -d -m 0700 -o "$OWNER" -g "$OWNER" "$DST"
install -m 0644 -o "$OWNER" -g "$OWNER" "$SRC/fullchain.pem" "$DST/fullchain.pem"
install -m 0600 -o "$OWNER" -g "$OWNER" "$SRC/privkey.pem" "$DST/privkey.pem"
