#!/usr/bin/env bash
# Install fpgaflow for the current user: release binaries in ~/.local/bin,
# the icon, and a KDE/freedesktop menu entry under "Engineering".
# No root needed.  `scripts/install-desktop.sh --uninstall` removes it all.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
conf="${XDG_CONFIG_HOME:-$HOME/.config}"
cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"

files=(
  "$bin/fpgaflow-gui"
  "$bin/fpgaflow"
  "$bin/fpgaflow-celoxica"
  "$bin/fpgaflow-player"
  "$data/applications/fpgaflow.desktop"
  "$data/applications/fpgaflow-player.desktop"
  "$data/desktop-directories/fpgaflow-engineering.directory"
  "$conf/menus/applications-merged/fpgaflow-engineering.menu"
)
refresh() {
  command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
  command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$data/icons/hicolor" || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -fv "${files[@]}"
  find "$data/icons/hicolor" -path '*/apps/fpgaflow.png' -print -delete 2>/dev/null || true
  refresh
  exit 0
fi

"$cargo" build --release --manifest-path "$repo/Cargo.toml" -p fpgaflow-gui -p fpgaflow-cli -p fpgaflow-celoxica -p fpgaflow-player
install -Dm755 "$repo/target/release/fpgaflow-gui" "$bin/fpgaflow-gui"
install -Dm755 "$repo/target/release/fpgaflow" "$bin/fpgaflow"
install -Dm755 "$repo/target/release/fpgaflow-celoxica" "$bin/fpgaflow-celoxica"
install -Dm755 "$repo/target/release/fpgaflow-player" "$bin/fpgaflow-player"

for d in "$repo"/data/icons/hicolor/*/; do
  install -Dm644 "$d/apps/fpgaflow.png" "$data/icons/hicolor/$(basename "$d")/apps/fpgaflow.png"
done
sed "s|@BIN@|$bin/fpgaflow-gui|" "$repo/data/desktop/fpgaflow.desktop" \
  | install -Dm644 /dev/stdin "$data/applications/fpgaflow.desktop"
sed "s|@BIN@|$bin/fpgaflow-player|" "$repo/data/desktop/fpgaflow-player.desktop" \
  | install -Dm644 /dev/stdin "$data/applications/fpgaflow-player.desktop"
install -Dm644 "$repo/data/desktop/fpgaflow-engineering.directory" \
  "$data/desktop-directories/fpgaflow-engineering.directory"
install -Dm644 "$repo/data/desktop/fpgaflow-engineering.menu" \
  "$conf/menus/applications-merged/fpgaflow-engineering.menu"
refresh
echo "installed: menu Engineering → FPGA Flow ($bin/fpgaflow-gui), RC10 Player ($bin/fpgaflow-player)"
