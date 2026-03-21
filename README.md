# wlgrid

A fast grid-based launcher for Wayland, inspired by Windows 10's Start menu. Built in Rust.

![screenshot](screenshot.png)

## Features
<100ms time-to-interactive (FS caching adds randomness)
designed with tablet/phone use in mind

**Mouse**
- Click and drag to rearrange tiles
- Right-click to remove a tile
- Click empty tile to open app picker

**Keyboard**
- Arrow keys to navigate tiles
- Enter to launch focused app
- Type to search:
  - Matches zoxide directories first
  - Falls back to search engines (you don't have to press the search button, "enter" will automatically execute the first search option in config)
  - Paths with `/` or `~` get tab completion and open directly

**Bottom bar**
- Customizable quick-action buttons (logout, reboot, etc.)

## Config

`~/.config/wlgrid/config.toml`

```toml
width = 7
height = 6
opacity = 1.0

search_engines = """
Brave = https://search.brave.com/search?q={}
Claude = https://claude.ai/new?q={}
DuckDuckGo = https://duckduckgo.com/?q={}
"""

[bottom_bar]
font = 30
options = """
󰍃 = hyprshutdown
󰑓 = systemctl reboot
󰐥 = systemctl poweroff
󰒲 = systemctl hibernate
󰤄 = systemctl suspend
"""
```

## Installation

### Debian/Ubuntu
```bash
sudo apt install build-essential cargo libwayland-dev libxkbcommon-dev
```
### Arch Linux
```bash
sudo pacman -S rust wayland libxkbcommon
```


### Debian/Ubuntu/Arch, most normal distros 
```bash
git clone https://github.com/whymusticode/wlgrid
cd wlgrid
cargo build --release
sudo cp target/release/wlgrid /usr/local/bin/
```


### Nix

```bash
# Run directly
nix run github:whymusticode/wlgrid

# Or install in a shell
nix shell github:whymusticode/wlgrid

# Or add to your flake inputs
{
  inputs.wlgrid.url = "github:whymusticode/wlgrid";
}
```

## Dependencies

- Wayland compositor
- zoxide (optional, for directory matching)
