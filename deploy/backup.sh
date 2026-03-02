#!/usr/bin/env bash
# deploy/backup.sh
#
# Backup script for Chan imageboard.
# Backs up: SQLite database (using SQLite's online backup API), uploaded files.
#
# Cron example (daily at 3am):
#   sudo crontab -e
#   0 3 * * * /usr/local/bin/chan-backup.sh >> /var/log/chan-backup.log 2>&1
#
# To restore:
#   sudo systemctl stop chan
#   cp /backup/chan/chan_YYYYMMDD.db /var/lib/chan/chan.db
#   tar -xzf /backup/chan/uploads_YYYYMMDD.tar.gz -C /
#   sudo systemctl start chan

set -euo pipefail

BACKUP_DIR="${CHAN_BACKUP_DIR:-/var/backup/chan}"
DB_PATH="${CHAN_DB:-/var/lib/chan/chan.db}"
UPLOAD_DIR="${CHAN_UPLOADS:-/var/lib/chan/uploads}"
KEEP_DAYS="${CHAN_BACKUP_KEEP:-14}"   # Keep 14 days of backups
DATE=$(date +%Y%m%d_%H%M%S)

echo "[$(date)] Starting Chan backup..."

# Create backup directory
mkdir -p "$BACKUP_DIR"

# ── Database backup (uses SQLite's .backup command for consistency) ──────────
DB_BACKUP="$BACKUP_DIR/chan_${DATE}.db"
echo "[$(date)] Backing up database to $DB_BACKUP"
sqlite3 "$DB_PATH" ".backup '$DB_BACKUP'"
gzip "$DB_BACKUP"
echo "[$(date)] Database backup complete: ${DB_BACKUP}.gz"

# ── Uploads backup (incremental using tar) ───────────────────────────────────
UPLOAD_BACKUP="$BACKUP_DIR/uploads_${DATE}.tar.gz"
echo "[$(date)] Backing up uploads to $UPLOAD_BACKUP"
tar -czf "$UPLOAD_BACKUP" -C "$(dirname "$UPLOAD_DIR")" "$(basename "$UPLOAD_DIR")" \
    --exclude='*.tmp'
echo "[$(date)] Uploads backup complete: $UPLOAD_BACKUP"

# ── Prune old backups ─────────────────────────────────────────────────────────
echo "[$(date)] Pruning backups older than $KEEP_DAYS days..."
find "$BACKUP_DIR" -name "chan_*.db.gz"          -mtime "+$KEEP_DAYS" -delete
find "$BACKUP_DIR" -name "uploads_*.tar.gz"      -mtime "+$KEEP_DAYS" -delete

# ── Report sizes ─────────────────────────────────────────────────────────────
echo "[$(date)] Backup complete."
du -sh "$BACKUP_DIR"

# ── Optional: copy to USB drive if mounted ───────────────────────────────────
# USB_MOUNT="/mnt/backup-usb"
# if mountpoint -q "$USB_MOUNT"; then
#     cp "${DB_BACKUP}.gz" "$USB_MOUNT/"
#     echo "[$(date)] Copied to USB drive at $USB_MOUNT"
# fi
