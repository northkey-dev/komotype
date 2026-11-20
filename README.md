# komotype

A gamified typing tracker for Windows, built for the [komorebi](https://github.com/LGUG2Z/komorebi) tiling window manager.

> **Status:** early design phase. Nothing usable yet. See [docs/spec.md](docs/spec.md) (German).

## What it does (planned)

- Live WPM, accuracy, streaks and personal records for **one keyboard of your choice**.
- XP, levels, daily/weekly quests, seasons, prestige and hidden achievements, to motivate you to stick with your new keyboard.
- Seamless display inside **komorebi-bar** via a widget in a komorebi fork.
- Optional **ZMK firmware module** that adds data only the keyboard knows: active layer, battery of both split halves, physical key heatmap, home-row-mod and combo stats.

## Components

| Component | Location |
|---|---|
| `komotyped`: background daemon (Raw Input, stats, game engine) | this repo |
| ZMK module (raw HID) | this repo (`firmware/`) |
| komorebi-bar widget | [northkey-dev/komorebi](https://github.com/northkey-dev/komorebi) (fork) |

## Try the daemon (work in progress)

Requires Rust (stable, MSVC toolchain).

```
cargo build --release
target\release\komotyped.exe pick          # press a key on your main keyboard
target\release\komotyped.exe               # run the daemon
target\release\komotyped.exe status --watch
```

Config: `%USERPROFILE%\.config\komotype\config.toml` · Data: `%LOCALAPPDATA%\komotype\komotype.db`.
Only counts are stored, never what you type.

## Credits

komotype would not exist without these projects:

- **[komorebi](https://github.com/LGUG2Z/komorebi)** by [LGUG2Z](https://github.com/LGUG2Z): the tiling window manager and komorebi-bar this project extends. The name *komotype* follows the `komo*` naming of the komorebi ecosystem (like [komokana](https://github.com/LGUG2Z/komokana)). If komorebi is useful to you, consider [sponsoring LGUG2Z](https://github.com/sponsors/LGUG2Z).
- **[ZMK Firmware](https://github.com/zmkfirmware/zmk)**: the keyboard firmware the optional module builds on.
- **[zmk-raw-hid](https://github.com/zzeneg/zmk-raw-hid)** by [zzeneg](https://github.com/zzeneg): raw HID transport between ZMK keyboards and the host.

komotype is an independent project and not affiliated with or endorsed by komorebi or its author.

## License

komotype's own code is licensed under the [MIT License](LICENSE).

The komorebi fork that hosts the bar widget stays under the [Komorebi License](https://github.com/LGUG2Z/komorebi/blob/master/LICENSE.md). It allows personal, non-commercial use and sharing of source code with changes, but **not** distributing prebuilt binaries. That's why the widget is provided as source only and you build the fork yourself.
