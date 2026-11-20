# komotype

A gamified typing tracker for Windows that lives in the [komorebi](https://github.com/LGUG2Z/komorebi) bar.
It is built to help you stick with one keyboard (say, your new split keyboard) by turning every day of
typing on it into progress.

## Features

- **Live stats in komorebi-bar:** WPM, active layer, level with XP bar, loyalty multiplier, words today and
  keyboard battery.
- **Tracks one keyboard of your choice.** Typing on other keyboards is only counted, to measure loyalty and
  your old keyboard's speed (the "rival").
- **Progression:** XP for every word, levels 1–100 with rank titles, prestige, daily quests sized to your
  own averages, a weekly challenge, quarterly seasons, a daily streak with freezes.
- **Loyalty multiplier:** days on which 95%+ of your keystrokes go to the main keyboard raise your XP
  multiplier up to ×2.0. Going back to the old keyboard resets it.
- **Hidden achievements:** 160+, spread over months and years. Locked ones show only their rarity, secret
  ones not even that. Definitions ship encrypted so the repository does not spoil them.
- **Windows notifications** for achievements, titles, records, weekly challenges and streak changes.
- **Optional ZMK module** for data only the keyboard knows: active layer, battery of both split halves,
  a per-key heatmap and home-row-mod tap/hold statistics.

**Privacy:** only counts are stored (per minute, per key, per day). komotype never records what you type
or in which order. Everything stays on your machine.

## How it fits together

```
 ZMK keyboard ──raw HID (USB/BT)──▶ komotyped ◀──named pipe──▶ komorebi-bar (fork) · Komotype widget
 any keyboard ──Raw Input────────▶ (daemon: stats, game, SQLite)
```

| Component | Where |
|---|---|
| `komotyped`, the daemon | this repo, `crates/komotyped` |
| ZMK module (optional) | this repo, `firmware/` (Zephyr module) |
| Bar widget | [northkey-dev/komorebi](https://github.com/northkey-dev/komorebi), branch `komotype` |

## Install

Requirements: Windows 10/11, komorebi, and for building: [Rust](https://rustup.rs) (stable, MSVC).

### 1. Daemon

Download `komotyped.exe` from the [releases](https://github.com/northkey-dev/komotype/releases), or build it:

```
cargo install --git https://github.com/northkey-dev/komotype komotyped
```

### 2. Bar with the widget

The komorebi license does not allow sharing prebuilt binaries of a modified komorebi-bar, so build it yourself:

```
git clone -b komotype https://github.com/northkey-dev/komorebi
cd komorebi
cargo build --release -p komorebi-bar
```

Add the widget to your `komorebi.bar.json` (all fields except `enable` are optional):

```json
{
  "Komotype": {
    "enable": true,
    "layer_names": ["Base", "Sym", "Num", "Nav"],
    "heatmap_rows": [12, 12, 14, 12],
    "key_labels": ["Tab", "Q", "W", "..."],
    "show_wpm": true,
    "show_layer": true,
    "show_level": true,
    "show_loyalty": true,
    "show_words_today": true,
    "show_battery": true,
    "autostart": true,
    "daemon_path": "C:\\path\\to\\komotyped.exe"
  }
}
```

Run `target\release\komorebi-bar.exe --config <your komorebi.bar.json>` instead of the official bar.
The widget starts `komotyped` automatically (from `PATH` or `daemon_path`).

### 3. Choose your keyboard

Click the widget → ⚙ tab → **Choose by pressing a key**, then press any key on your keyboard.
Or on the command line: `komotyped pick`.

### 4. Optional: ZMK module

Add both modules to your `config/west.yml`:

```yaml
manifest:
  remotes:
    - name: zzeneg
      url-base: https://github.com/zzeneg
    - name: northkey-dev
      url-base: https://github.com/northkey-dev
  projects:
    - name: zmk-raw-hid
      remote: zzeneg
      revision: main
    - name: komotype
      remote: northkey-dev
      revision: main
```

Then add the `komotype` shield to the **central** half only (or to a non-split board) in `build.yaml`,
e.g. `shield: my_keyboard_left komotype`. After flashing, remove and re-pair the keyboard in the Windows
Bluetooth settings once, so Windows sees the new HID interface.
Protocol details: [docs/protocol.md](docs/protocol.md).

## Configuration

`%USERPROFILE%\.config\komotype\config.toml`, written by `pick` / the settings tab, editable by hand:

```toml
streak_threshold = 2000   # keystrokes per day that keep the streak alive

[main]
vid = 0x1D50
pid = 0x615E
product = "Cornix"        # optional, case-insensitive substring

[notifications]
enabled = true
achievements = true
titles = true
records = true
weekly = true
streak = true
quests = false
```

Data: `%LOCALAPPDATA%\komotype\komotype.db` (SQLite). Log: `%LOCALAPPDATA%\komotype\komotyped.log`.

## Command line

```
komotyped                 run the daemon (normally started by the widget)
komotyped devices         list keyboards
komotyped pick            choose the main keyboard by pressing a key
komotyped status --watch  live status in the terminal
komotyped notify-test     show a test notification
```

## Credits

komotype would not exist without these projects:

- **[komorebi](https://github.com/LGUG2Z/komorebi)** by [LGUG2Z](https://github.com/LGUG2Z): the tiling window manager and komorebi-bar this project extends. The name *komotype* follows the `komo*` naming of the komorebi ecosystem (like [komokana](https://github.com/LGUG2Z/komokana)). If komorebi is useful to you, consider [sponsoring LGUG2Z](https://github.com/sponsors/LGUG2Z).
- **[ZMK Firmware](https://github.com/zmkfirmware/zmk)**: the keyboard firmware the optional module builds on.
- **[zmk-raw-hid](https://github.com/zzeneg/zmk-raw-hid)** by [zzeneg](https://github.com/zzeneg): raw HID transport between ZMK keyboards and the host.

komotype is an independent project and not affiliated with or endorsed by komorebi or its author.

## License

komotype's own code is licensed under the [MIT License](LICENSE).

The komorebi fork that hosts the bar widget stays under the [Komorebi License](https://github.com/LGUG2Z/komorebi/blob/master/LICENSE.md). It allows personal, non-commercial use and sharing of source code with changes, but **not** distributing prebuilt binaries. That's why the widget is provided as source only and you build the fork yourself.
