# Conway's Game Of Life Wallpaper

## Installation

### Automatic user install (recommended)

Requirements: a Rust toolchain (`cargo`) and a Wayland compositor that supports `zwlr_layer_shell_v1`.
From the project directory, run:

```sh
./install.sh
```

The script builds the optimized binary and installs it to `~/.local/bin`, the man page to `~/.local/share/man/man1`, and example TOML files to `~/.local/share/life-wallpaper/config_example`. It creates `~/.config/life-wallpaper/config.toml` from the default template only if that file does not already exist, so existing settings are preserved.

If `life-wallpaper` is not found after installation, ensure `~/.local/bin` is on your `PATH`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

To use the installed manual page:

```sh
man life-wallpaper
```

The script supports these environment overrides: `PREFIX` (default `~/.local`), `BIN_DIR`, `DATA_DIR`, `MAN_DIR`, `CONFIG_DIR`, and `CARGO`.

### Manual install

```sh
cargo build --release
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/man/man1" "$HOME/.config/life-wallpaper"
install -m 755 target/release/life-wallpaper "$HOME/.local/bin/life-wallpaper"
install -m 644 man/life-wallpaper.1 "$HOME/.local/share/man/man1/life-wallpaper.1"
install -m 644 config_example/config.default.toml "$HOME/.config/life-wallpaper/config.toml" # only if you don't already have one
```

If you already have a config file, skip the last command to avoid overwriting your settings. To run without installing, use `cargo run --release -- --preset ambient`.

## Usage

```bash
cargo build --release
cargo run
cargo test
```

## Presets

Select a built-in preset from the command line:

```bash
life-wallpaper --preset default
life-wallpaper --preset ambient
life-wallpaper --preset matrix-rain
life-wallpaper --preset neon-brain
life-wallpaper --preset minimal
```

Preset values are loaded first, then an existing config file can override them, and command-line flags take final precedence. If the config file is missing, it is created from the default template, while the selected preset remains active for that first run. Individual flags can override preset values, for example `life-wallpaper --preset neon-brain --fps 6`.

The manual page is in `man/life-wallpaper.1`. View it with `man ./man/life-wallpaper.1`.

## Configuration hot reload

On first launch, the app creates `~/.config/life-wallpaper/config.toml` from the bundled `config_example/config.default.toml` template (or creates the file selected by `--config`). Parent directories are created as needed. If the config file cannot be created, the app logs a warning and continues with the built-in `Config::default()` values. Those built-in values match the bundled template.

The wallpaper watches the selected config file automatically (by checking its modification time on each tick). Save `~/.config/life-wallpaper/config.toml` or the file passed with `--config` and the running wallpaper applies the changes without restarting. CLI flags continue to override config-file values. `SIGHUP` reload remains supported:

```bash
pkill -HUP life-wallpaper
```

## License

MIT
## Example configurations

The `config_example/` directory includes optional presets you can try:

- `config.default.toml` — balanced default setup; used to create the config on first launch.
- `ambient.toml` — slower, calmer animation with fewer visual elements.
- `matrix-rain.toml` — green palette with code rain and status text.
- `neon-brain.toml` — brighter neon colors using Brian's Brain.
- `minimal.toml` — a clean look without trails, rain, overlays, patterns, or stamps.

To try a preset, copy its settings into `~/.config/life-wallpaper/config.toml` (or the file you selected with `--config`). You can also copy the preset over that file, then edit it as desired. The running wallpaper automatically reloads changes when the file is saved.
