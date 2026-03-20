# wlgrid
Tired of letters everywhere on your screen? Not enough launchers to choose from in the linux ecosystem? 

introducing ...

A blazing fast grid-based win10-nostalgic launcher for Wayland in rust. click and drag. right click remove. click empty tile to add. fully customizeable bottom bar

![screenshot](screenshot.png)


## config.toml
```shell
mode = "grid"   # "fuzzel" or "grid"
alpha = 0.08 # only for fuzzel mode, this is forgetting factor 
width = 6 # in icons 
height = 6
opacity = 1
icon_size = 42.0
custom_entries = []
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