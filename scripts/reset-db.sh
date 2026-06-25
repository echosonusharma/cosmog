#!/usr/bin/env bash
set -e

DB_DIR="$HOME/.local/share/com.sonus.cosmog"

rm -f "$DB_DIR/cosmog.sqlite" \
       "$DB_DIR/cosmog.sqlite-wal" \
       "$DB_DIR/cosmog.sqlite-shm"

echo "cosmog db reset"
