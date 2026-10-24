//! The Add games screen: games found on the drives, one row each. UI-free so it can be
//! unit-tested; main.rs runs the scan, performs the effects and mirrors this into the window.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::Action;
use crate::library::discover::Candidate;

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

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Add(usize),
    Ignore(usize),
    Restore(usize),
    Rescan,
}

#[derive(Default)]
pub struct AddGames {
    pub scanning: bool,
    /// Folders looked at so far in the running scan.
    pub folders: usize,
    pub rows: Vec<Row>,
    pub focus: usize,
}

impl AddGames {
    pub fn start_scan(&mut self) {
        self.scanning = true;
        self.folders = 0;
        self.rows.clear();
        self.focus = 0;
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

    pub fn handle(&mut self, action: Action) -> Option<Effect> {
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
            _ => {}
        }
        None
    }
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
        screen.start_scan();
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
        screen.start_scan();
        assert_eq!(screen.handle(Action::TabNext), None, "already scanning");
        assert_eq!(
            screen.handle(Action::Confirm),
            None,
            "no rows while scanning"
        );
    }
}
