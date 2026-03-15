# RDP Anchor

**A GUI launcher for multi-monitor RDP connections that survive monitor ID changes across reboots.**

## What it solves

Monitor IDs returned by `mstsc /l` (0, 1, 2...) can change after reboots or dock reconnections.
The `selectedmonitors:s:0,1` field in `.rdp` files is ID-based, so shifted IDs cause connections to the wrong monitors.

**Key insight**: Monitor IDs are unstable, but **physical screen coordinates and resolutions are stable**.
This tool identifies monitors by coordinates and automatically resolves the correct IDs at connection time.

## Features

- **Multi-host management**: Register hosts by .rdp file, view as card-based UI
- **Display profiles**: Define named profiles like "Left 2 screens", "Right 2 screens", "All screens"
- **Host x Profile**: Set a default profile per host, switch at connection time
- **Connection guard**: Confirmation dialog when reconnecting to an already-connected host
- **Preserves original .rdp**: Only rewrites monitor settings in a temp copy; all other settings are kept
- **Drag & drop**: Drop .rdp files onto the window to add hosts
- **Contiguity check**: Prevents creating profiles with non-adjacent monitors
- **Zero dependencies**: Single binary (WebView2 is built into Windows 10/11)
- **Bilingual UI**: Japanese and English, switchable in settings

## Usage

### 1. Initial setup

1. Launch the app - connected monitors are auto-detected
2. Settings (⚙) → "Profiles" tab → Create a profile
   - Click monitors to select/deselect
   - Double-click to set as Primary
3. Add a host
   - "+ Add Host" → specify an .rdp file
   - Or drag & drop an .rdp file onto the window
   - Select the default profile

### 2. Daily use

- Click the "Connect" button on a host card
- Double-click a card to connect
- Switch profiles via the dropdown on each card
- Drag the grip (☰) to reorder cards (order is saved)
- Works seamlessly after reboots (IDs are resolved each time)

### 3. When your monitor layout changes

If you change the physical display arrangement (Windows Settings > Display > Arrangement):
1. Settings → Monitors tab → "Auto Detect" to re-detect
2. Review/adjust your profiles

## Configuration

Saved at `%APPDATA%/rdp-anchor/config.json`. Can be edited manually:

```json
{
  "language": "en",
  "monitors": {
    "mon-0": { "name": "left 1920x1080", "left": -1920, "top": 0, "width": 1920, "height": 1080 },
    "mon-1": { "name": "center 2560x1440", "left": 0, "top": 0, "width": 2560, "height": 1440 }
  },
  "profiles": {
    "left-two": {
      "name": "Left 2 screens",
      "monitor_ids": ["mon-0", "mon-1"],
      "primary": "mon-1"
    }
  },
  "hosts": [
    {
      "id": "dev",
      "name": "Dev Server",
      "rdp_file": "C:\\Users\\me\\dev.rdp",
      "default_profile": "left-two",
      "color": "#5b9aff"
    }
  ]
}
```

## Troubleshooting

**Q: Monitors are not detected**
→ Run `mstsc /l` manually in a command prompt and check the output. If a dialog appears, it's working.

**Q: "Profile not found" error when connecting**
→ The host's default profile was deleted. Reassign it in settings.

**Q: Connection works but displays on the wrong monitors**
→ Display arrangement may have changed. Settings → Monitors → Auto Detect to update.

---

This software is currently available free of charge. All rights reserved.
