# wlgrid

A snappy grid-based launcher for Wayland, inspired by Windows 10's Start menu, built in Rust.

I made this because 
1) I use a tablet
2) I'm sick of everything being text in linux
3) nwg-drawers is the closest thing, but this is faster and has better features for me

DISCLAIMER: this is vibe-coded to a large degree. I'm a dev of 15 years, but I don't really know rust or wayland. I have carefully read all the code and I've been using this for months. 

![wlgrid launcher demo](output.gif)

the gif compressed this so badly, it really does look a lot better:

![wlgrid launcher demo](screenshot.png)

## Features
- <100ms time-to-interactive 
- <4000 LOC
- HiDPI / fractional scaling support
- Designed with touchscreen use in mind
- Nerd Font integration:
  - Icons in the bottom bar (see config below)
  - Put a Nerd Font glyph in a desktop entry name and it'll be used as the icon if no image icon is found

**Mouse**
- Click and drag to rearrange tiles (layout persists across launches)
- Right-click to remove a tile
- Click an empty tile to open the app picker — a full searchable view of all installed apps

**Keyboard**
- Arrow keys to navigate tiles
- Arrow down from the bottom row enters the bottom bar; arrow up returns to the grid
- Left/Right to navigate bottom bar items; Enter to activate
- Enter to launch the focused app
- Type to search:
  - Matches desktop entries, zoxide directories, search engines, and files (hotkeys: `/`, `.`, `~`)
  - Search sources and order are configurable — open a GH issue if you want a specific source integrated into search (e.g. krunner)

**Bottom bar**
- Customizable quick-action buttons (logout, reboot, any shell command, etc.)

## Config

`~/.config/wlgrid/config.toml`

> Note: wlgrid also stores layout/cache state in this folder.

```toml
width = 7 # in icons
height = 7
start_col = 3
start_row = 3
icon_size = 42.0 # in pixels
tile_color = "#ffffff"

# FYI toml requires a 0 in front of decimal
dim = 0.4
corner_radius = 10
accent_hue_delta = 18.0
accent_amount = 1
panel_color = "#000000"
panel_alpha = 1.0
tile_alpha = 0.075
border_color = "#10130c"
border_alpha = 0.12
show_tile_outlines = true 

search = "[desktop,folders]"
search_engines = """
Duck = https://duckduckgo.com/?q={}
Nix = https://search.nixos.org/packages?query={}
"""   

# good session cleanup is usually custom to your WM, e.g. "swaymsg exit" or "hyprshutdown" 
[bottom_bar]
font = 30
options = """
󰍃 = swaymsg exit
󰑓 = systemctl reboot 
󰐥 = systemctl poweroff 
󰒲 = systemctl hibernate
󰤄 = systemctl suspend
"""
```

## Installation

### Arch Linux (AUR)
```bash
yay -S wlgrid-git
```

### Nix
```bash
# Run directly
nix run github:whymusticode/wlgrid

# Or add to your flake inputs
{
  inputs.wlgrid.url = "github:whymusticode/wlgrid";
}
```

### Build from source

**Debian/Ubuntu**
```bash
sudo apt install build-essential cargo clang mold libwayland-dev libxkbcommon-dev
```

**Arch Linux**
```bash
sudo pacman -S rust clang mold wayland libxkbcommon
```

Then build and install:
```bash
git clone https://github.com/whymusticode/wlgrid
cd wlgrid
cargo build --release
sudo cp target/release/wlgrid /usr/local/bin/
```

## Runtime dependencies

- Wayland compositor
- OpenGL/EGL (Mesa or any vendor driver)
- `zoxide` (optional — enables directory search)
