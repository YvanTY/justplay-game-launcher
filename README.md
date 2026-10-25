# JustPlay - Game Launcher

**Pick up any controller, choose any game, play it on any screen. Just play.**

JustPlay is a lightweight, controller-first game launcher for Windows with a PS5-style home
screen. Everything works from the couch with a gamepad: browsing your games, adding new ones,
choosing art, setting up controllers, picking the screen and the sound output. When a game
starts, the launcher gets out of the way and uses almost nothing until you come back.

## Why it exists

Launchers like Playnite start games well, but managing them with a controller means getting
up for the mouse. JustPlay was built the other way round: the gamepad comes first, and the
mouse and keyboard are there only as a fallback. It also takes care of the little things that
usually stop you from just playing: a cheap pad that games don't recognise, a game that opens
on the wrong screen, sound coming out of the wrong speakers.

## Features

- **PS5-style home:** tabs per platform, a row of games, full-screen background art,
  the game's logo, recently played cards and per-game music.
- **All your games in one place:** PC games, PS3 games imported from RPCS3, and any emulator
  you configure (PS2, Switch...). Add games from inside the launcher by scanning a drive or a
  folder. Games on an unplugged drive are greyed out and come back when the drive does.
- **Art found for you:** covers, backgrounds and logos from Steam, the Microsoft Store and
  PCGamingWiki, plus SteamGridDB with a free key. A picker lets you choose among many
  versions for each game.
- **Any controller:** Xbox, PlayStation, Switch Pro and generic USB pads, hot-plugged. PC games
  get a virtual Xbox controller driven by your pad, so Xbox-only games accept any pad.
- **Controllers screen:** test every button, remap buttons (replace what a button does, or add
  a keyboard key to it), or remap the whole pad one button after another. A check catches a
  stick moved by accident.
- **Screens:** choose the screen the launcher shows on and the screen games play on, or show
  the same picture on all screens. Games that insist on their own monitor are moved to the
  chosen one, and your screen layout is restored afterwards.
- **Sound:** a game's sound follows its screen (the TV's speakers for a game on the TV), or
  goes to an output you choose, such as a headset. Other apps keep their sound where it was.
- **Light:** about 0% CPU when idle; while a game runs the launcher stops drawing and frees
  its images.
- **Mouse too:** click, scroll and right-click to go back, for when the pad is out of reach.

## Requirements

- Windows 10 or 11, 64-bit
- [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) for the virtual Xbox controller
  (optional: without it, games see your pad as it is)
- To build: [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Windows 10/11 SDK
  (installed with the Visual Studio C++ build tools)

## Build and run

```powershell
git clone https://github.com/YvanTY/justplay-game-launcher.git
cd justplay-game-launcher
cargo build --release
.\target\release\game-launcher.exe
```

SDL3 is included in `vendor/` and copied next to the executable by the build.

## Setting up your games

On first start, JustPlay creates `%APPDATA%\GameLauncher\config.toml` with commented examples.
You can also add games from the launcher itself (quick panel > **Add games**).

```toml
[[games]]
name = "God of War"
exe = 'E:\Games\God of War\GoW.exe'

[rpcs3]                       # PS3 games come from RPCS3's own list
exe = 'E:\Emulators\RPCS3\rpcs3.exe'

[[systems]]                   # any other emulator: ROMs found by extension
name = "Switch"
emulator = 'E:\Emulators\Ryujinx\Ryujinx.exe'
args = '--fullscreen "{rom}"'
roms = 'E:\Roms\Switch'
extensions = ["nsp", "xci"]

[art]                         # optional, for many more covers and backgrounds
steamgriddb_key = "your key"
```

## Controls

| Button | Action |
|---|---|
| D-pad / left stick | Move |
| A | Play / confirm |
| B | Back |
| Y | Game options |
| LB / RB | Previous / next tab |
| LT / RT | Jump a page |
| Start | Quick panel (settings, controllers, add games, power) |
| Hold Start + Select (in a game) | Bring the launcher back |

Keyboard: arrows, Enter, Esc, Tab, Page Up / Page Down, F1. Mouse: click, wheel, right-click.

## Good to know

- Routing a game's sound to one output uses the same Windows feature as the volume mixer's
  per-app setting. Windows offers no official API for it, so a future Windows update could
  change it; the launcher logs what happens in `%APPDATA%\GameLauncher\launcher.log`.
- Games in exclusive full screen may not let themselves be moved to another screen;
  borderless and windowed games do.

## Built with

[Rust](https://www.rust-lang.org) · [Slint](https://slint.dev) (UI) ·
[SDL3](https://libsdl.org) (controllers, zlib license) ·
[ViGEm](https://github.com/nefarius/ViGEmBus) (virtual Xbox controller) ·
[Symphonia](https://github.com/pdeljanov/Symphonia) (music)

## License

[MIT](LICENSE) © 2026 YvanTY
