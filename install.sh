#!/bin/sh
set -eu

# Install life-wallpaper for the current user. Run from any directory.
# Optional overrides: PREFIX, CONFIG_DIR, MAN_DIR, CARGO.

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CARGO=${CARGO:-cargo}
PREFIX=${PREFIX:-"${HOME}/.local"}
BIN_DIR=${BIN_DIR:-"${PREFIX}/bin"}
DATA_DIR=${DATA_DIR:-"${PREFIX}/share/life-wallpaper"}
MAN_DIR=${MAN_DIR:-"${PREFIX}/share/man/man1"}
CONFIG_DIR=${CONFIG_DIR:-"${XDG_CONFIG_HOME:-${HOME}/.config}/life-wallpaper"}

if ! command -v "$CARGO" >/dev/null 2>&1; then
    echo "error: cargo was not found. Install Rust/Cargo first (https://rustup.rs/)." >&2
    exit 1
fi

if [ ! -f "$ROOT/Cargo.toml" ]; then
    echo "error: couldn't find Cargo.toml beside install.sh" >&2
    exit 1
fi

printf '%s\n' "Building life-wallpaper in release mode..."
(cd "$ROOT" && "$CARGO" build --release)

mkdir -p "$BIN_DIR" "$DATA_DIR/config_example" "$MAN_DIR" "$CONFIG_DIR"
install -m 755 "$ROOT/target/release/life-wallpaper" "$BIN_DIR/life-wallpaper"

# Install examples for reference, and make the default config available on disk.
for file in "$ROOT"/config_example/*.toml; do
    [ -f "$file" ] || continue
    install -m 644 "$file" "$DATA_DIR/config_example/$(basename "$file")"
done

# Keep user edits intact. Create the config only if it doesn't already exist.
if [ ! -e "$CONFIG_DIR/config.toml" ]; then
    if [ -f "$ROOT/config_example/config.default.toml" ]; then
        install -m 644 "$ROOT/config_example/config.default.toml" "$CONFIG_DIR/config.toml"
    else
        echo "warning: default config template not found; the app will use built-in defaults" >&2
    fi
else
    echo "Keeping existing config: $CONFIG_DIR/config.toml"
fi

if [ -f "$ROOT/man/life-wallpaper.1" ]; then
    install -m 644 "$ROOT/man/life-wallpaper.1" "$MAN_DIR/life-wallpaper.1"
fi

printf '\nInstalled life-wallpaper to: %s/life-wallpaper\n' "$BIN_DIR"
printf 'Config: %s/config.toml\n' "$CONFIG_DIR"
printf 'Examples: %s/config_example/\n' "$DATA_DIR"
printf 'Man page: %s/life-wallpaper.1\n' "$MAN_DIR"
case ":${PATH}:" in
    *":$BIN_DIR:"*) ;;
    *) printf 'Note: add %s to your PATH if the command is not found.\n' "$BIN_DIR" ;;
esac
printf '\nTry: life-wallpaper --preset ambient\n'
printf 'Manual: man life-wallpaper (or man %s/life-wallpaper.1)\n' "$MAN_DIR"
