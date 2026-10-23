# PRD — Game Launcher (working name)

## Why
Playnite launches games fine, but everything else (menus, settings, managing the library) is frustrating with a controller. This launcher is **controller-first**: anything you can do in it, you can do from the couch with a gamepad. It stays out of the way while a game is running.

## Principles
1. **Controller-first.** Every screen and every action works with a gamepad. A mouse is never required. Keyboard is a fallback.
2. **Featherweight.** Idle CPU ≈ 0%. While a game runs, the launcher stops drawing, frees its images and pauses audio.
3. **Any controller.** Xbox, PlayStation, Switch Pro and generic USB/DirectInput pads (e.g. UCom), with hot-plugging.

## Look & feel — BuzzTV structure, PS5 atmosphere
```
┌──────────────────────────────────────────────────────────────────┐
│ [LB]  Home   PC   SNES   PS2   Favorites  [RB]          22:41  ☰ │  ← tabs (LB/RB)
│                                                                  │
│  ELDEN RING                                   ┌──── quick panel ─┐│
│  PC · Last played yesterday                   │ ⚙ Settings  ⟳ Scan││  ← Menu button
│                                               │ 🎮 Pads   ⏻ Power ││     opens it
│  ┌━━━━━━━━┓ ┌────────┐ ┌────────┐ ┌────────┐ │ ✕ Exit           ││
│  ┃ focused┃ │        │ │        │ │        │ └──────────────────┘│
│  ┗━━━━━━━━┛ └────────┘ └────────┘ └────────┘                     │  ← grid of wide tiles
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐                     │
│  └────────┘ └────────┘ └────────┘ └────────┘                     │
│   (focused game's hero art fills the whole background, dimmed)   │
└──────────────────────────────────────────────────────────────────┘
```
- **Background:** the focused game's hero art fills the screen behind a dark gradient and crossfades when focus moves (PS5).
- **Music:** if the focused game has music, it fades in. Otherwise a quiet ambient bed plays (PS5).
- **Tabs:** **Home** (recently played first, then everything), **PC** (Steam + manual), one tab per emulator system, and **Favorites**. Empty tabs are hidden (BuzzTV).
- **Grid:** wide 16:9 tiles with a thick bright outline and a slight scale-up on focus (BuzzTV).
- **Quick panel:** a slide-in panel of large square buttons (BuzzTV): Settings, Rescan library, Controller test, Power (Sleep / Restart / Shut down), Exit launcher, plus a clock.
- **Game sheet:** Y on a game opens Play / Add to (or Remove from) Favorites.

### Controls
| Button | Action |
|---|---|
| D-pad / left stick | Move |
| A | Launch / confirm |
| B | Back / close |
| Y | Game sheet |
| LB / RB | Previous / next tab |
| Menu (Start) | Quick panel |
| Hold Start + Select for 1 s (in game) | Bring launcher to front |

Keyboard fallback: arrows, Enter, Esc, Tab / Shift+Tab, F1 (panel).

## Stack
| Layer | Choice |
|---|---|
| Language | Rust (stable, MSVC toolchain) |
| UI | Slint (GPU-rendered, declarative, redraws only on change) |
| Controllers | SDL3 gamepad subsystem only (generic pad mapping database) |
| Audio | kira (crossfades, UI sounds) |
| Storage | `config.toml` (user-edited) + `state.json` (favorites, last played), with no database |
| Sounds | Synthesized in code, so no copyrighted assets |

## v0.1 scope (MVP)
1. The home screen, tabs, grid, background art, quick panel and game sheet as described above.
2. **Steam library:** detect installed games automatically from Steam's files. Use the artwork Steam already caches locally (tile = header image, background = hero image), so no API keys are needed.
3. **Emulators:** in `config.toml`, each system lists its name, ROM folder, file extensions, emulator exe and argument template (e.g. `-L cores/snes.dll "{rom}"`). Art is loaded from `<artdir>/<rom name>.png|jpg` if present. Without art, a styled title tile is shown.
4. **Manual games:** list any .exe in `config.toml` with a name and optional art.
5. **Launch & return:** launch a game, hide the launcher and drop to near-zero resources. Detect the game's exit (process watch, including games started through Steam) and come back automatically. Hold Start + Select to force-return.
6. **Settings page (controller-driven):** master volume, music volume, UI sounds on/off and art dim level. Left/right change values. Saved immediately.
7. **Per-game music:** `music/<game id>.ogg|mp3` fades in on focus.
8. **Controller test page:** live inputs for each connected pad, to verify generic pads.

## Not in v0.1
Epic, GOG, Xbox/Game Pass, EA and Ubisoft libraries · online metadata or art scraping · video trailers · an in-game overlay · themes · adding games or editing rows from inside the app (v0.2) · installers or auto-update.

## Build order (each milestone is committed and tested before the next)
1. **Skeleton:** Rust project, fullscreen Slint window, SDL3 controller input driving focus on a dummy grid, keyboard fallback.
2. **Library:** config loading, Steam scan, emulator and manual entries, and the art cache, with unit tests.
3. **Home UI:** tabs, grid, background art crossfade, and the game sheet.
4. **Launch & return:** process launch and tracking, hide/sleep while in game, the Start + Select return combo, and last-played tracking.
5. **Sound:** synthesized UI sounds, ambient bed and per-game music crossfade.
6. **Quick panel, Settings, Controller test, Power actions.**
7. **Performance pass:** measure against the targets and fix.

## Performance targets
- Cold start to interactive: < 1 s with 300 games (art cache warm)
- Idle on home screen: < 1% CPU, < 120 MB RAM
- While a game runs: ~0% CPU, < 40 MB RAM
- Release exe + SDL3.dll: < 25 MB

## Definition of done (v0.1)
- `cargo build --release`, `cargo clippy -- -D warnings` and `cargo test` all pass.
- Unit tests cover Steam manifest parsing, emulator command templates and config loading.
- Manual checklist passed on Yvan's PC: navigate, launch, auto-return and use the quick panel with (a) an Xbox-style pad and (b) the UCom pad. Measure CPU and RAM with Task Manager on the home screen and during a game.
- Committed via `/commit`.
