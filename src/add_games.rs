//! The Add games screen: games found on the drives, one row each, plus choosing where to look
//! (every drive, one drive, or a folder picked with a controller-friendly browser). UI-free so
//! it can be unit-tested; main.rs runs the scan, performs the effects and mirrors this into the
//! window.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Action;
use crate::library::discover::Candidate;
use crate::library::drives;

/// Rows skipped by LT / RT.
const PAGE: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    New,
    Added,
    /// Not suggested again until restored with Y.
    Ignored,
}

pub struct Row {
    pub candidate: Candidate,
    /// Which of the candidate's exes will be launched.
    pub exe: usize,
    pub status: Status,
}

impl Row {
    pub fn exe_path(&self) -> &PathBuf {
        &self.candidate.exes[self.exe]
    }
}

/// Where a scan looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    AllDrives,
    /// A drive root (`E:\`) or any folder.
    Folder(PathBuf),
}

impl Place {
    pub fn describe(&self) -> String {
        match self {
            Place::AllDrives => "all drives".into(),
            Place::Folder(path) => path.display().to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Results,
    /// Choosing where to look: all drives, a drive, a recent folder, or "Choose a folder…".
    Places,
    /// Inside the folder browser; None is the list of drives.
    Browse(Option<PathBuf>),
    /// The emulators set up, and "Add an emulator…".
    Emulators,
}

/// What the folder browser picks a folder for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A place to look for PC games.
    Games,
    /// The folder an emulator lives in.
    EmulatorProgram,
    /// The folder with that emulator's games (the system's name, for the screen).
    EmulatorGames(String),
}

impl Purpose {
    /// The browser row and hint that use the folder being shown.
    pub fn use_label(&self) -> &'static str {
        match self {
            Purpose::Games => "Look in this folder",
            Purpose::EmulatorProgram => "The emulator is in this folder",
            Purpose::EmulatorGames(_) => "The games are in this folder",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    AllDrives,
    Drive,
    Folder,
    /// Opens the folder browser.
    Browse,
    /// In the browser: look in the folder being shown.
    LookHere,
    /// An emulator that is set up (`path`: its program); `removable` when set up here.
    System {
        removable: bool,
    },
    /// Starts setting up an emulator.
    AddEmulator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceRow {
    pub kind: PlaceKind,
    pub path: Option<PathBuf>,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Add(usize),
    Ignore(usize),
    Restore(usize),
    /// Scan again where the last scan looked.
    Rescan,
    /// Open the list of places (main supplies the remembered folders).
    ChoosePlace,
    /// Scan this place (and remember it).
    Scan(Place),
    /// List this folder's subfolders off the UI thread (a sleeping external drive can take
    /// seconds to answer), then call `set_listing`.
    List(PathBuf),
    /// B on the results: leave the screen.
    Close,
    /// The folder an emulator should be in (main looks for one it knows).
    EmulatorFolder(PathBuf),
    /// The folder with the emulator's games: the setup is complete.
    EmulatorGamesFolder(PathBuf),
    /// Take out the emulator set up from the launcher whose program this is.
    RemoveEmulator(PathBuf),
    /// Show the emulators again (main supplies the list).
    ShowEmulators,
}

pub struct AddGames {
    pub scanning: bool,
    /// Folders looked at so far in the running scan.
    pub folders: usize,
    pub rows: Vec<Row>,
    pub focus: usize,
    pub mode: Mode,
    pub place: Place,
    /// Rows of the place list or the folder browser.
    pub places: Vec<PlaceRow>,
    pub place_focus: usize,
    /// Folders chosen before, shown again in the place list.
    recent: Vec<PathBuf>,
    /// The browser waits for a folder listing.
    pub listing: bool,
    pub purpose: Purpose,
}

impl Default for AddGames {
    fn default() -> AddGames {
        AddGames {
            scanning: false,
            folders: 0,
            rows: Vec::new(),
            focus: 0,
            mode: Mode::Results,
            place: Place::AllDrives,
            places: Vec::new(),
            place_focus: 0,
            recent: Vec::new(),
            listing: false,
            purpose: Purpose::Games,
        }
    }
}

impl AddGames {
    pub fn start_scan(&mut self, place: Place) {
        self.purpose = Purpose::Games;
        self.scanning = true;
        self.folders = 0;
        self.rows.clear();
        self.focus = 0;
        self.mode = Mode::Results;
        self.place = place;
    }

    /// New games first (alphabetical, as found), ignored ones after them.
    pub fn set_results(&mut self, found: Vec<Candidate>, ignored: &BTreeSet<PathBuf>) {
        self.scanning = false;
        let (mut rows, hidden): (Vec<Row>, Vec<Row>) = found
            .into_iter()
            .map(|candidate| Row {
                status: if ignored.contains(&candidate.folder) {
                    Status::Ignored
                } else {
                    Status::New
                },
                candidate,
                exe: 0,
            })
            .partition(|row| row.status == Status::New);
        rows.extend(hidden);
        self.rows = rows;
        self.focus = 0;
    }

    pub fn new_count(&self) -> usize {
        self.rows.iter().filter(|r| r.status == Status::New).count()
    }

    pub fn added_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.status == Status::Added)
            .count()
    }

    /// The emulators set up (title, detail, program, set up here?), then "Add an emulator…".
    pub fn open_emulators(&mut self, systems: Vec<(String, String, PathBuf, bool)>) {
        let mut rows: Vec<PlaceRow> = systems
            .into_iter()
            .map(|(title, detail, program, removable)| PlaceRow {
                kind: PlaceKind::System { removable },
                path: Some(program),
                title,
                detail,
            })
            .collect();
        rows.push(PlaceRow {
            kind: PlaceKind::AddEmulator,
            path: None,
            title: "Add an emulator…".into(),
            detail: "PCSX2, Ryujinx, yuzu, Dolphin, DuckStation, PPSSPP or Cemu".into(),
        });
        // Starts on "Add an emulator…": that's what this screen is opened for.
        self.place_focus = rows.len() - 1;
        self.places = rows;
        self.mode = Mode::Emulators;
        self.purpose = Purpose::Games;
    }

    /// Picks the emulator's games folder next (after its program was found).
    pub fn choose_games_folder(&mut self, system: &str) -> Option<Effect> {
        self.purpose = Purpose::EmulatorGames(system.to_string());
        self.browse(None)
    }

    /// The list of places: every drive, the folders used before (`recent`), and the browser.
    pub fn open_places(&mut self, recent: &[PathBuf]) {
        self.recent = recent.to_vec();
        let drives = drives::scannable();
        let mut rows = vec![PlaceRow {
            kind: PlaceKind::AllDrives,
            path: None,
            title: "All drives".into(),
            detail: drives
                .iter()
                .map(|l| format!("{l}:"))
                .collect::<Vec<_>>()
                .join("  "),
        }];
        rows.extend(drives.iter().map(|&letter| drive_row(letter)));
        rows.extend(recent.iter().filter(|p| p.is_dir()).map(|path| PlaceRow {
            kind: PlaceKind::Folder,
            path: Some(path.clone()),
            title: folder_name(path),
            detail: path.display().to_string(),
        }));
        rows.push(PlaceRow {
            kind: PlaceKind::Browse,
            path: None,
            title: "Choose a folder…".into(),
            detail: "Browse your drives with the controller".into(),
        });
        // Start on the place the last scan used.
        self.place_focus = rows
            .iter()
            .position(|row| match (&self.place, &row.path) {
                (Place::AllDrives, None) => row.kind == PlaceKind::AllDrives,
                (Place::Folder(place), Some(path)) => place == path,
                _ => false,
            })
            .unwrap_or(0);
        self.places = rows;
        self.mode = Mode::Places;
    }

    /// Shows the drives (None), or starts showing `dir`: "Look in this folder" at once, its
    /// subfolders when the listing arrives (`Effect::List`).
    pub fn browse(&mut self, dir: Option<PathBuf>) -> Option<Effect> {
        self.place_focus = 0;
        self.mode = Mode::Browse(dir.clone());
        match dir {
            None => {
                self.listing = false;
                self.places = drives::scannable().into_iter().map(drive_row).collect();
                None
            }
            Some(dir) => {
                self.listing = true;
                self.places = vec![PlaceRow {
                    kind: PlaceKind::LookHere,
                    path: Some(dir.clone()),
                    title: self.purpose.use_label().into(),
                    detail: dir.display().to_string(),
                }];
                Some(Effect::List(dir))
            }
        }
    }

    /// The subfolders of `dir` arrived; ignored if the browser moved on meanwhile.
    pub fn set_listing(&mut self, dir: &Path, folders: Vec<PathBuf>) {
        if self.mode != Mode::Browse(Some(dir.to_path_buf())) {
            return;
        }
        self.listing = false;
        self.places.truncate(1);
        self.places.extend(folders.into_iter().map(|path| PlaceRow {
            kind: PlaceKind::Folder,
            detail: path.display().to_string(),
            title: folder_name(&path),
            path: Some(path),
        }));
    }

    pub fn handle(&mut self, action: Action) -> Option<Effect> {
        match self.mode.clone() {
            Mode::Results => self.handle_results(action),
            Mode::Places => self.handle_places(action),
            Mode::Browse(dir) => self.handle_browse(action, dir),
            Mode::Emulators => self.handle_emulators(action),
        }
    }

    fn handle_emulators(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Back => return Some(Effect::Close),
            Action::Confirm => {
                if self.places.get(self.place_focus)?.kind == PlaceKind::AddEmulator {
                    self.purpose = Purpose::EmulatorProgram;
                    return self.browse(None);
                }
            }
            Action::Options => {
                let row = self.places.get(self.place_focus)?;
                if row.kind == (PlaceKind::System { removable: true }) {
                    return row.path.clone().map(Effect::RemoveEmulator);
                }
            }
            _ => self.move_place_focus(action),
        }
        None
    }

    /// The folder picked in the browser, for whatever it was picked for.
    fn use_folder(&self, folder: PathBuf) -> Effect {
        match self.purpose {
            Purpose::Games => Effect::Scan(Place::Folder(folder)),
            Purpose::EmulatorProgram => Effect::EmulatorFolder(folder),
            Purpose::EmulatorGames(_) => Effect::EmulatorGamesFolder(folder),
        }
    }

    fn handle_results(&mut self, action: Action) -> Option<Effect> {
        let last = self.rows.len().saturating_sub(1);
        match action {
            Action::Up => self.focus = self.focus.saturating_sub(1),
            Action::Down => self.focus = (self.focus + 1).min(last),
            Action::PagePrev => self.focus = self.focus.saturating_sub(PAGE),
            Action::PageNext => self.focus = (self.focus + PAGE).min(last),
            Action::Left | Action::Right => {
                let row = self.rows.get_mut(self.focus)?;
                let count = row.candidate.exes.len();
                if row.status != Status::New || count < 2 {
                    return None;
                }
                row.exe = if action == Action::Right {
                    (row.exe + 1) % count
                } else {
                    (row.exe + count - 1) % count
                };
            }
            Action::Confirm => {
                let row = self.rows.get(self.focus)?;
                return (row.status == Status::New).then_some(Effect::Add(self.focus));
            }
            Action::Options => {
                return match self.rows.get(self.focus)?.status {
                    Status::New => Some(Effect::Ignore(self.focus)),
                    Status::Ignored => Some(Effect::Restore(self.focus)),
                    Status::Added => None,
                };
            }
            Action::TabNext if !self.scanning => return Some(Effect::Rescan),
            Action::TabPrev => return Some(Effect::ChoosePlace),
            Action::Back => return Some(Effect::Close),
            _ => {}
        }
        None
    }

    fn handle_places(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Back => self.mode = Mode::Results,
            Action::Confirm => {
                let row = self.places.get(self.place_focus)?.clone();
                return match row.kind {
                    PlaceKind::AllDrives => Some(Effect::Scan(Place::AllDrives)),
                    PlaceKind::Browse => self.browse(None),
                    _ => row.path.map(|p| Effect::Scan(Place::Folder(p))),
                };
            }
            _ => self.move_place_focus(action),
        }
        None
    }

    fn handle_browse(&mut self, action: Action, dir: Option<PathBuf>) -> Option<Effect> {
        match action {
            Action::Back => match dir.as_deref().and_then(Path::parent) {
                Some(parent) => return self.browse(Some(parent.to_path_buf())),
                // From a drive root (or the drive list) back to the places.
                None if dir.is_some() => return self.browse(None),
                // Picking for an emulator: back to the emulators.
                None if self.purpose != Purpose::Games => return Some(Effect::ShowEmulators),
                None => {
                    let recent = std::mem::take(&mut self.recent);
                    self.open_places(&recent);
                }
            },
            Action::Options => return dir.map(|d| self.use_folder(d)),
            Action::Confirm => {
                let row = self.places.get(self.place_focus)?.clone();
                match (row.kind, row.path) {
                    (PlaceKind::LookHere, Some(path)) => return Some(self.use_folder(path)),
                    (_, Some(path)) => return self.browse(Some(path)),
                    _ => {}
                }
            }
            _ => self.move_place_focus(action),
        }
        None
    }

    fn move_place_focus(&mut self, action: Action) {
        let last = self.places.len().saturating_sub(1);
        match action {
            Action::Up => self.place_focus = self.place_focus.saturating_sub(1),
            Action::Down => self.place_focus = (self.place_focus + 1).min(last),
            Action::PagePrev => self.place_focus = self.place_focus.saturating_sub(PAGE),
            Action::PageNext => self.place_focus = (self.place_focus + PAGE).min(last),
            _ => {}
        }
    }
}

/// A folder's subfolders, sorted by name, hidden and system ones left out. May block for
/// seconds on a sleeping drive: call it off the UI thread.
pub fn list_folders(dir: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| !hidden(p))
                .collect()
        })
        .unwrap_or_default();
    folders.sort_by_cached_key(|p| folder_name(p).to_lowercase());
    folders
}

fn drive_row(letter: char) -> PlaceRow {
    let root = PathBuf::from(format!("{letter}:\\"));
    PlaceRow {
        kind: PlaceKind::Drive,
        title: match drives::label(letter) {
            Some(label) => format!("{letter}:  {label}"),
            None => format!("{letter}:"),
        },
        detail: "Drive".into(),
        path: Some(root),
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// System and hidden folders ($Recycle.Bin, System Volume Information, .git, the protective
/// autorun.inf folders some tools create) aren't offered.
fn hidden(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    let name = folder_name(path).to_lowercase();
    let flagged = fs::metadata(path).is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0);
    flagged || name.starts_with('$') || name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str, exes: &[&str]) -> Candidate {
        Candidate {
            title: title.into(),
            folder: PathBuf::from(format!(r"E:\Games\{title}")),
            exes: exes.iter().map(PathBuf::from).collect(),
        }
    }

    fn screen() -> AddGames {
        let mut screen = AddGames::default();
        screen.start_scan(Place::AllDrives);
        let ignored = BTreeSet::from([PathBuf::from(r"E:\Games\Bully")]);
        screen.set_results(
            vec![
                candidate("Bully", &["Bully.exe"]),
                candidate("Hades", &["Hades.exe"]),
                candidate("TLOU", &["tlou-i.exe", "launcher.exe"]),
            ],
            &ignored,
        );
        screen
    }

    #[test]
    fn ignored_folders_sink_to_the_bottom() {
        let screen = screen();
        let titles: Vec<&str> = screen
            .rows
            .iter()
            .map(|r| r.candidate.title.as_str())
            .collect();
        assert_eq!(titles, ["Hades", "TLOU", "Bully"]);
        assert_eq!(screen.rows[2].status, Status::Ignored);
        assert!(!screen.scanning);
        assert_eq!(screen.new_count(), 2);
    }

    #[test]
    fn left_and_right_pick_another_exe() {
        let mut screen = screen();
        screen.handle(Action::Down); // TLOU
        assert!(screen.rows[1].exe_path().ends_with("tlou-i.exe"));
        screen.handle(Action::Right);
        assert!(screen.rows[1].exe_path().ends_with("launcher.exe"));
        screen.handle(Action::Right);
        assert!(screen.rows[1].exe_path().ends_with("tlou-i.exe"), "wraps");
        screen.handle(Action::Up); // Hades has one exe: nothing to pick
        screen.handle(Action::Left);
        assert_eq!(screen.rows[0].exe, 0);
    }

    #[test]
    fn a_adds_y_ignores_or_restores_rb_scans_again() {
        let mut screen = screen();
        assert_eq!(screen.handle(Action::Confirm), Some(Effect::Add(0)));
        screen.rows[0].status = Status::Added;
        assert_eq!(screen.handle(Action::Confirm), None, "already added");
        assert_eq!(screen.handle(Action::Options), None);
        screen.handle(Action::PageNext); // last row: Bully, ignored
        assert_eq!(screen.focus, 2);
        assert_eq!(screen.handle(Action::Confirm), None);
        assert_eq!(screen.handle(Action::Options), Some(Effect::Restore(2)));
        assert_eq!(screen.handle(Action::TabNext), Some(Effect::Rescan));
        screen.start_scan(Place::AllDrives);
        assert_eq!(screen.handle(Action::TabNext), None, "already scanning");
        assert_eq!(
            screen.handle(Action::Confirm),
            None,
            "no rows while scanning"
        );
        assert_eq!(screen.handle(Action::Back), Some(Effect::Close));
    }

    #[test]
    fn places_list_drives_recent_folders_and_the_browser() {
        let mut screen = screen();
        assert_eq!(screen.handle(Action::TabPrev), Some(Effect::ChoosePlace));
        let recent = std::env::temp_dir();
        screen.open_places(std::slice::from_ref(&recent));
        assert_eq!(screen.mode, Mode::Places);
        assert_eq!(screen.places[0].kind, PlaceKind::AllDrives);
        assert_eq!(screen.place_focus, 0, "starts on the place last used");
        assert!(screen.places.iter().any(|r| r.kind == PlaceKind::Drive));
        assert!(
            screen
                .places
                .iter()
                .any(|r| r.path.as_ref() == Some(&recent))
        );
        assert_eq!(screen.places.last().unwrap().kind, PlaceKind::Browse);
        assert_eq!(
            screen.handle(Action::Confirm),
            Some(Effect::Scan(Place::AllDrives))
        );
        screen.handle(Action::Back);
        assert_eq!(screen.mode, Mode::Results);
    }

    #[test]
    fn emulators_are_set_up_with_the_browser_and_removed_with_y() {
        let mut screen = screen();
        let rpcs3 = PathBuf::from(r"E:\Emu\rpcs3.exe");
        let pcsx2 = PathBuf::from(r"E:\Emu\PCSX2\pcsx2-qt.exe");
        screen.open_emulators(vec![
            ("PS3".into(), "RPCS3".into(), rpcs3, false),
            ("PS2".into(), "PCSX2".into(), pcsx2.clone(), true),
        ]);
        assert_eq!(screen.mode, Mode::Emulators);
        assert_eq!(screen.places.last().unwrap().kind, PlaceKind::AddEmulator);
        assert_eq!(screen.place_focus, 2, "starts on Add an emulator");
        // Only emulators set up here can be removed.
        screen.handle(Action::Up);
        assert_eq!(
            screen.handle(Action::Options),
            Some(Effect::RemoveEmulator(pcsx2))
        );
        screen.handle(Action::Up);
        assert_eq!(screen.handle(Action::Options), None, "config.toml's stay");
        // Adding one: the browser picks the emulator's folder, then its games folder.
        screen.handle(Action::Down);
        screen.handle(Action::Down);
        assert_eq!(screen.handle(Action::Confirm), None);
        assert_eq!(screen.mode, Mode::Browse(None));
        let dir = std::env::temp_dir();
        screen.browse(Some(dir.clone()));
        assert_eq!(screen.places[0].title, "The emulator is in this folder");
        assert_eq!(
            screen.handle(Action::Confirm),
            Some(Effect::EmulatorFolder(dir.clone()))
        );
        screen.choose_games_folder("PS2");
        screen.browse(Some(dir.clone()));
        assert_eq!(
            screen.handle(Action::Options),
            Some(Effect::EmulatorGamesFolder(dir))
        );
        // B from the drive list goes back to the emulators, not to the game places.
        screen.browse(None);
        assert_eq!(screen.handle(Action::Back), Some(Effect::ShowEmulators));
    }

    #[test]
    fn the_browser_opens_folders_looks_in_them_and_goes_up() {
        let root = std::env::temp_dir().join(format!("gl-browse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Jeu").join("Blur")).unwrap();
        fs::create_dir_all(root.join("$Recycle.Bin")).unwrap();
        let mut screen = screen();
        assert_eq!(
            screen.browse(Some(root.clone())),
            Some(Effect::List(root.clone()))
        );
        assert!(screen.listing);
        screen.set_listing(&root, list_folders(&root));
        assert!(!screen.listing);
        let titles: Vec<&str> = screen.places.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Look in this folder", "Jeu"],
            "system folders hidden"
        );
        screen.handle(Action::Down);
        assert_eq!(
            screen.handle(Action::Confirm),
            Some(Effect::List(root.join("Jeu")))
        );
        assert_eq!(screen.mode, Mode::Browse(Some(root.join("Jeu"))));
        // A listing that arrives after moving on is dropped.
        screen.set_listing(&root, list_folders(&root));
        assert_eq!(screen.places.len(), 1);
        assert_eq!(
            screen.handle(Action::Options),
            Some(Effect::Scan(Place::Folder(root.join("Jeu")))),
            "Y looks here"
        );
        assert_eq!(
            screen.handle(Action::Confirm),
            Some(Effect::Scan(Place::Folder(root.join("Jeu")))),
            "so does the first row"
        );
        screen.handle(Action::Back);
        assert_eq!(screen.mode, Mode::Browse(Some(root.clone())));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn leaving_the_browser_shows_the_whole_place_list_again() {
        let mut screen = screen();
        screen.open_places(&[]);
        let full = screen.places.len();
        screen.place_focus = full - 1; // Choose a folder…
        screen.handle(Action::Confirm);
        assert_eq!(screen.mode, Mode::Browse(None));
        screen.handle(Action::Back);
        assert_eq!(screen.mode, Mode::Places);
        assert_eq!(screen.places.len(), full);
        assert_eq!(screen.places[0].kind, PlaceKind::AllDrives);
    }
}
