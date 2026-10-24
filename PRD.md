# PRD — Game Launcher (working name)

## Why
Playnite launches games fine, but everything else (menus, settings, managing the library) is frustrating with a controller. This launcher is **controller-first**: anything you can do in it, you can do from the couch with a gamepad. It stays out of the way while a game is running.

## Principles
1. **Controller-first.** Every screen and every action works with a gamepad. A mouse is never required. Keyboard is a fallback.
2. **Featherweight.** Idle CPU ≈ 0%. While a game runs, the launcher stops drawing, frees its images and pauses audio.
3. **Any controller.** Xbox, PlayStation, Switch Pro and generic USB/DirectInput pads (e.g. UCom), with hot-plugging.

## Look & feel — PS5 home layout (BuzzTV quick panel)
Changed after the step 1 test: Yvan prefers the full PS5 layout over the BuzzTV grid. Reference: the PS5 home screen screenshots Yvan shared (Ratchet & Clank / Spider-Man home).
```
┌──────────────────────────────────────────────────────────────────┐
│  Home   PC 12   SNES 30   PS2 7   Favorites        ⚙   10:42 PM  │  ← text tabs, active = bold white
│ ┏━━━━━━┓┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐                 │  ← small rounded square icons,
│ ┃      ┃└──┘└──┘└──┘└──┘└──┘└──┘└──┘└──┘└──┘└──┘                 │     selected one larger on a
│ ┗━━━━━━┛ Ratchet & Clank: Rift Apart                             │     dark glass backing + its name
│                                                                  │
│                                                                  │
│   ██████ RATCHET & CLANK ██████                                  │  ← big game logo (or big title)
│                                                                  │
│   ( ▶  Play )  ( ⋯ )    PC · Last played yesterday              │  ← action buttons + info
│                                                                  │
│   Recently played                                                │
│   ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐         │  ← wide 16:9 cards
│   └────────┘ └────────┘ └────────┘ └────────┘ └────────┘         │
│      (selected game's hero art fills the whole screen)           │
└──────────────────────────────────────────────────────────────────┘
```
- **Top bar:** plain text tabs like PS5's "Games / Media". The active tab is bold white, the others dim, with small game counts. On the right are a gear (opens the quick panel) and the clock. No pills or boxes. No search in v0.1.
- **Bottom row:** "Recently played" as wide cards using each game's art (up to 8). Down from the buttons moves into it, and A launches. *(Builder's call: PS5 shows news cards here. Recently played is the nearest useful thing for a local launcher.)*
- **Background:** the selected game's hero art fills the screen behind a soft dark gradient (left and bottom) and crossfades when the selection moves. No art: a blurred, darkened version of its icon.
- **Game row:** square icons (PS3 ICON0, emulator box art or user art, cropped to fill). The selected icon is larger, with a bright outline and its name to the right. The row slides so the selection always sits in the first slot, as on PS5.
- **Logo & actions:** the game's logo image (from the art folder), otherwise its title in large type. Below it, a big **Play** button and a **⋯** button that opens the game sheet. Then an info line (platform, last played).
- **Music:** if the selected game has music, it fades in. Otherwise a quiet ambient bed plays.
- **Tabs:** **Home** (recently played first, then everything), **PC** (manual entries), one tab per emulator system, and **Favorites**. Empty tabs are hidden. Each shows its game count.
- **Quick panel (from BuzzTV):** a slide-in panel of large square buttons: Settings, Rescan library, Controller test, Power (Sleep / Restart / Shut down), Exit launcher, plus a clock.
- **Game sheet:** ⋯ or Y opens Play / Add to (or Remove from) Favorites.

### Controls
| Button | Action |
|---|---|
| Left / right (D-pad or stick) | Move along the game row |
| Down / up | Icon row → Play / ⋯ buttons → Recently played cards, and back. Up from the icon row → tab bar |
| A | Launch (on an icon or Play) / confirm |
| B | Back / close |
| Y | Game sheet |
| LB / RB | Previous / next tab |
| LT / RT | Jump a page (8 games) left / right |
| Menu (Start) | Quick panel |
| Hold Start + Select for 1 s (in game) | Bring launcher to front |

Keyboard fallback: arrows, Enter, Esc/Backspace, Space (game sheet), Tab / Shift+Tab, Page Up / Page Down, F1 (panel).

## Stack
| Layer | Choice |
|---|---|
| Language | Rust (stable, MSVC toolchain) |
| UI | Slint (GPU-rendered, declarative, redraws only on change) |
| Controllers | SDL3 gamepad subsystem only (generic pad mapping database) |
| Audio | SDL3 audio stream + a small built-in mixer (replaced kira: the device can be fully paused, so no audio CPU during games) |
| Xbox pad emulation | vigem-client (pure Rust) talking to the ViGEmBus driver |
| Storage | `config.toml` (user-edited) + `state.json` (favorites, last played), with no database |
| Sounds | Synthesized in code, so no copyrighted assets |

## v0.1 scope (MVP)
1. The home screen, tabs, game row, background art, quick panel and game sheet as described above.
2. **RPCS3 (PS3) import:** read RPCS3's own game list (`config/games.yml`) plus games installed inside RPCS3 (`dev_hdd0/game`, category HG). Titles come from `PARAM.SFO`; art from the game's own `ICON0.PNG` (icon) and `PIC1.PNG` (background). `PIC0.PNG` is not used as a logo: it is often an empty transparent placeholder (God of War III), so PS3 games show their title text unless a logo is supplied in the art folder. A `hide` list in the config removes entries. *(Replaced the Steam scanner: Steam isn't installed on Yvan's PC.)*
3. **Emulators:** in `config.toml`, each system lists its name, ROM folder, file extensions, emulator exe and argument template (e.g. `-L cores/snes.dll "{rom}"`). Art is loaded from `<artdir>/<rom name>.png|jpg` if present. Without art, a styled title tile is shown.
4. **Manual games:** list any .exe in `config.toml` with a name and optional art.
5. **Launch & return:** launch a game, hide the launcher and drop to near-zero resources. Detect the game's exit (process watch, including games that start a second process) and come back automatically. Hold Start + Select to force-return.
6. **Settings page (controller-driven):** master volume, music volume, UI sounds on/off and art dim level. Left/right change values. Saved immediately.
7. **Per-game music:** `music/<game id>.ogg|mp3` fades in on focus.
8. **Controller test page:** live inputs for each connected pad, to verify generic pads.
9. **Automatic Xbox controller emulation (replaces x360ce):** while a PC game runs, the launcher plugs in a virtual Xbox 360 pad (ViGEmBus) and drives it from the user's pad, using SDL's mapping database, so no manual mapping is needed. On by default for PC games and off for emulators, which read pads directly; `xinput = false` turns it off per game. Pads that are already Xbox pads are never used as the source. The launcher ignores its own virtual pad, so presses never arrive twice. While the launcher is shown over the game, the game receives a neutral pad. *(Added after Yvan's request: his UCom pad is DirectInput-only and God of War is XInput-only.)*

## Not in v0.1
Steam, Epic, GOG, Xbox/Game Pass, EA and Ubisoft libraries · online metadata or art scraping · video trailers · an in-game overlay · themes · adding games or editing rows from inside the app (v0.2) · installers or auto-update.

## Build order (each milestone is committed and tested before the next)
1. **Skeleton:** Rust project, fullscreen Slint window, SDL3 controller input driving focus on a dummy grid, keyboard fallback.
2. **Library:** config loading, RPCS3 import, folder-scanned emulators and manual entries, with unit tests. (The art cache moves to step 7.)
3. **Home UI (PS5 layout):** tabs, game row, logo and action buttons, background art crossfade, and the game sheet.
4. **Launch & return:** process launch and tracking, hide/sleep while in game, the Start + Select return combo, and last-played tracking.
5. **Sound:** synthesized UI sounds, ambient bed and per-game music crossfade.
6. **Quick panel, Settings, Controllers screen, Power actions.**
   - **Controllers screen** (per connected pad, remembered per controller model): a picture of the pad in the middle, its buttons listed around it in columns like x360ce (left: triggers, bumpers, Back/Start, D-pad, left stick; right: face buttons, right stick). Up/Down moves through the list and the selected button **glows on the picture**. Mouse works too.
   - **Extra triggers:** any button can get extra inputs that also act as it, either a keyboard key or another pad button (for example keyboard Insert also presses L3 when the pad's L3 is broken). These apply to the virtual Xbox pad, so they work in every game that uses translation.
   - **Live test:** pressing a button on the pad lights it on the picture.
7. **Performance pass:** measure against the targets and fix.

## Performance targets
- Cold start to interactive: < 1 s with 300 games (art cache warm)
- Idle on home screen: < 1% CPU, < 120 MB RAM
- While a game runs: ~0% CPU, < 40 MB RAM
- Release exe + SDL3.dll: < 25 MB

## Definition of done (v0.1)
- `cargo build --release`, `cargo clippy -- -D warnings` and `cargo test` all pass.
- Unit tests cover PARAM.SFO and games.yml parsing, emulator command templates and config loading.
- Manual checklist passed on Yvan's PC: navigate, launch, auto-return and use the quick panel with (a) an Xbox-style pad and (b) the UCom pad. Measure CPU and RAM with Task Manager on the home screen and during a game.
- Committed via `/commit`.

## v0.2: library management (built while Yvan was away; decisions are the builder's)
Yvan asked for: adding games from inside the launcher, drive scanning (external drives included), games on a disconnected drive shown greyed out with a message, and automatic art (logo, background, video, sound). He handed every decision to the builder.

1. **Games on a disconnected drive stay listed, greyed out.** Every game knows the paths it needs (its exe, or the emulator plus the game file). If one is missing and its drive letter isn't connected, the tile is dimmed and Play says which drive to connect, using the drive's remembered name (e.g. "Connect drive E: (Nouveau nom)"). Missing files on a connected drive show "files not found" for PC games; scanner games (RPCS3, emulators) whose files were deleted still drop out with a warning. Scanner games are cached in `games-cache.json` (title, launch command, a copy of the icon), so PS3 and emulator games keep their name and icon while their drive is unplugged. Drives are checked every 2 s while the launcher is visible; plugging a drive in lights its games up without a restart.
2. **Automatic art from Steam's public store data (no account, no API key).** For PC games missing an icon, background or logo, the launcher searches the Steam store by title and accepts only an exact title match (ignoring case, punctuation, ™/®, roman vs. arabic numerals and common edition suffixes such as "Remastered" or "GOTY"); a near miss such as "Dante's Inferno" vs. "Dantes Bullet Inferno" is rejected, because wrong art is worse than none. It downloads the portrait cover (600x900), the 4K background (3840x1240) and the transparent logo into `art\<title>\`, never overwriting files already there. Each game is looked up once (remembered in `state.json`); network failures are retried on the next start. Downloads use Windows' built-in WinHTTP, so no HTTP library is added.
3. **Sharper backgrounds without extra memory.** Backgrounds are decoded on a background thread and shrunk to the screen's size (same lossless rule as icons), so 4K art costs no more than 1080p art and selecting a game never freezes the UI while it decodes.
4. **Add games screen (quick panel → Add games).** Scans every fixed and removable drive in the background and lists folders that look like PC games: an exe plus game markers (Steam/Epic/GOG/Ubisoft runtime DLLs, Bink video, Unity or Unreal layout, DLSS/FSR/Oodle libraries, redist folders, big data archives). Installers, uninstallers, crash reporters, redistributables, Windows folders and the emulators already configured are skipped. The main exe is the largest remaining one (ties go to the shorter name); Left/Right picks another exe if the guess is wrong. Titles come from the exe's product name, else the folder name cleaned of repack/scene tags. Controls: A adds (art is fetched right away), Y ignores a folder so it isn't suggested again (Y again restores it), RB scans again, B goes back. Added games are saved in `state.json` (`added_games`), so `config.toml` is never rewritten and keeps its comments.
5. **Remove from library** in the game sheet for games added from the launcher (files are never touched).

**After Yvan's first test (he asked for these; decisions are the builder's):**
6. **Art for games Steam doesn't sell.** Seven of eight games added from his H: drive got no art: delisted games (PES 2017/2019/2021, Blur) aren't in Steam's store search, FIFA 19 was never on Steam, and titles differ ("Grand Theft Auto V" is now "… V Enhanced"). Matching now also accepts more edition names (Enhanced, Legacy, Season Update…) and, for titles of four words or more, an added subtitle (never a sequel number, an add-on, or a choice between two subtitles). When Steam's search misses, PCGamingWiki's public pages give the Steam app id of delisted games (their images remain on Steam's server) and box art for others; old games without library images get a store screenshot as background and the banner as cover. Earlier misses are retried once.
7. **Where to look (Add games → LB).** All drives, one drive (by name), folders used before, or "Choose a folder…", a controller folder browser (A opens, Y or the first row looks there, B goes up). The place is remembered. Listings run off the UI thread so a sleeping external drive doesn't freeze the screen; hidden and system folders aren't listed.
8. **Choose art (game menu → Change art).** Three rows (cover, background, logo) of every picture found: the art in use, Steam images for the best store matches, screenshots, the banner, PCGamingWiki's cover, and None. Previews are light versions downloaded in parallel; the picked one shows at once and its sharp version replaces it when it arrives. The same picture at two sizes is offered once (visual fingerprint). The first image ever replaced is kept in `_original`; None writes a `<slot>.none` marker that automatic art respects (no logo shows the title, no background a plain backdrop).
9. **Mouse.** Hidden while a pad or keyboard is used, shown when the mouse moves. Click selects, clicking the selection activates it (a stray click never launches a game); buttons and menu items act at once; right click goes back; the wheel scrolls; every button hint is clickable.

**Left out on purpose:**
- *Video trailers:* playing video needs a decoder pipeline (Media Foundation or ffmpeg) and costs real CPU/GPU while browsing, against the launcher's first goal (lightweight). Yvan said it was fine to skip if difficult.
- *Automatic music:* there is no free, keyless source of per-game music. Dropping a file in `art\<title>\` still works.
- *Renaming games in the launcher:* needs an on-screen keyboard; titles come from the exe or Steam and can be changed in `config.toml`.
