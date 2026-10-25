//! On-screen keyboard for typing with a controller (renaming games). Four rows of ten
//! characters, then Shift, Space, Delete, Cancel and Done, each two columns wide.

use crate::Action;

const CHARS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl'", "zxcvbnm-:."];
const WIDE: [Key; 5] = [Key::Shift, Key::Space, Key::Delete, Key::Cancel, Key::Done];
/// Titles longer than this don't fit anywhere on screen.
pub const MAX_LEN: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Shift,
    Space,
    Delete,
    Cancel,
    Done,
}

impl Key {
    pub fn label(self, shift: bool) -> String {
        match self {
            Key::Char(c) if shift => c.to_uppercase().collect(),
            Key::Char(c) => c.to_string(),
            Key::Shift => "Shift".into(),
            Key::Space => "Space".into(),
            Key::Delete => "Delete".into(),
            Key::Cancel => "Cancel".into(),
            Key::Done => "Done".into(),
        }
    }
}

/// A key and where it is drawn: row, first column, width in columns.
pub struct Cap {
    pub key: Key,
    pub row: usize,
    pub col: usize,
    pub width: usize,
}

/// Every key, row by row.
pub fn caps() -> Vec<Cap> {
    let mut caps: Vec<Cap> = CHARS
        .iter()
        .enumerate()
        .flat_map(|(row, chars)| {
            chars.chars().enumerate().map(move |(col, c)| Cap {
                key: Key::Char(c),
                row,
                col,
                width: 1,
            })
        })
        .collect();
    caps.extend(WIDE.iter().enumerate().map(|(i, &key)| Cap {
        key,
        row: CHARS.len(),
        col: i * 2,
        width: 2,
    }));
    caps
}

pub enum Outcome {
    /// The text to keep (trimmed; may be empty).
    Done(String),
    Cancel,
}

pub struct Keyboard {
    pub text: String,
    pub shift: bool,
    /// The selected key, as an index into `caps()`.
    pub focus: usize,
    caps: Vec<Cap>,
}

impl Keyboard {
    pub fn new(text: &str) -> Keyboard {
        let caps = caps();
        // Starts on Done: renaming often means fixing a letter or two, then confirming.
        let focus = caps.iter().position(|c| c.key == Key::Done).unwrap_or(0);
        Keyboard {
            text: text.chars().take(MAX_LEN).collect(),
            shift: false,
            focus,
            caps,
        }
    }

    /// A controller (or arrow keys): move, type the selected key (A), delete (B), shift (Y),
    /// space (RB), done (Start). Returns whether the text or the selection changed, and an
    /// outcome when the keyboard closes.
    pub fn handle(&mut self, action: Action) -> (bool, Option<Outcome>) {
        match action {
            Action::Up | Action::Down | Action::Left | Action::Right => (self.step(action), None),
            Action::Confirm => self.press(self.caps[self.focus].key),
            Action::Back => self.press(Key::Delete),
            Action::Options => self.press(Key::Shift),
            Action::TabNext => self.press(Key::Space),
            Action::Menu => self.press(Key::Done),
            _ => (false, None),
        }
    }

    pub fn press(&mut self, key: Key) -> (bool, Option<Outcome>) {
        match key {
            Key::Char(c) => {
                let c = if self.shift {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                (self.type_char(c), None)
            }
            Key::Space => (self.type_char(' '), None),
            Key::Shift => {
                self.shift = !self.shift;
                (true, None)
            }
            Key::Delete => (self.text.pop().is_some(), None),
            Key::Cancel => (false, Some(Outcome::Cancel)),
            Key::Done => (false, Some(Outcome::Done(self.text.trim().to_string()))),
        }
    }

    /// Typed on a physical keyboard: characters, "\n" Enter, "\u{1b}" Escape, "\u{8}" Backspace.
    pub fn type_text(&mut self, text: &str) -> (bool, Option<Outcome>) {
        match text {
            "\n" | "\r" => self.press(Key::Done),
            "\u{1b}" => self.press(Key::Cancel),
            "\u{8}" => self.press(Key::Delete),
            _ => {
                let mut changed = false;
                for c in text.chars().filter(|c| is_typeable(*c)) {
                    changed |= self.type_char(c);
                }
                (changed, None)
            }
        }
    }

    fn type_char(&mut self, c: char) -> bool {
        if self.text.chars().count() >= MAX_LEN || (c == ' ' && self.text.ends_with(' ')) {
            return false;
        }
        self.text.push(c);
        true
    }

    /// Moves to the neighbouring key; between a narrow and a wide row the column decides.
    fn step(&mut self, action: Action) -> bool {
        let current = &self.caps[self.focus];
        let (row, col) = (current.row, current.col);
        let target = match action {
            Action::Left => self.key_at(row, col.checked_sub(1)),
            Action::Right => self.key_at(row, Some(col + current.width)),
            Action::Up => row.checked_sub(1).and_then(|r| self.key_at(r, Some(col))),
            Action::Down => self.key_at(row + 1, Some(col)),
            _ => None,
        };
        match target {
            Some(index) if index != self.focus => {
                self.focus = index;
                true
            }
            _ => false,
        }
    }

    fn key_at(&self, row: usize, col: Option<usize>) -> Option<usize> {
        let col = col?;
        self.caps
            .iter()
            .position(|c| c.row == row && (c.col..c.col + c.width).contains(&col))
    }
}

/// Characters a title can hold: no control characters or the private keys Slint uses for
/// arrows and function keys.
fn is_typeable(c: char) -> bool {
    !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard(text: &str) -> Keyboard {
        Keyboard::new(text)
    }

    fn selected(k: &Keyboard) -> Key {
        k.caps[k.focus].key
    }

    #[test]
    fn starts_on_done_and_moves_between_rows() {
        let mut k = keyboard("");
        assert_eq!(selected(&k), Key::Done);
        // Done spans columns 8-9: above it is column 8 of "zxcvbnm-:.".
        k.handle(Action::Up);
        assert_eq!(selected(&k), Key::Char(':'));
        k.handle(Action::Left);
        k.handle(Action::Left);
        assert_eq!(selected(&k), Key::Char('m'));
        // Column 6 is Cancel (columns 6-7) in the bottom row.
        k.handle(Action::Down);
        assert_eq!(selected(&k), Key::Cancel);
        k.handle(Action::Left);
        assert_eq!(selected(&k), Key::Delete);
        assert!(!k.handle(Action::Down).0, "nothing below the last row");
        k.handle(Action::Up);
        assert_eq!(
            selected(&k),
            Key::Char('b'),
            "up from a wide key: its first column"
        );
    }

    #[test]
    fn types_shifts_deletes_and_finishes() {
        let mut k = keyboard("God of Wa");
        k.press(Key::Char('r'));
        assert_eq!(k.text, "God of War");
        k.handle(Action::Options);
        k.press(Key::Char('i'));
        k.press(Key::Char('i'));
        k.handle(Action::Back);
        k.handle(Action::Options);
        k.handle(Action::TabNext);
        k.handle(Action::TabNext);
        assert_eq!(k.text, "God of WarI ", "one space, not two");
        match k.handle(Action::Menu).1 {
            Some(Outcome::Done(text)) => assert_eq!(text, "God of WarI"),
            _ => panic!("Start finishes"),
        }
    }

    #[test]
    fn a_physical_keyboard_types_too() {
        let mut k = keyboard("");
        k.type_text("Ratchet & Clank");
        k.type_text("\u{f700}");
        assert_eq!(k.text, "Ratchet & Clank", "arrow keys don't type");
        k.type_text("\u{8}");
        assert_eq!(k.text, "Ratchet & Clan");
        assert!(matches!(k.type_text("\u{1b}").1, Some(Outcome::Cancel)));
        assert!(matches!(k.type_text("\n").1, Some(Outcome::Done(_))));
    }

    #[test]
    fn titles_have_a_length_limit() {
        let mut k = keyboard(&"x".repeat(MAX_LEN + 5));
        assert_eq!(k.text.chars().count(), MAX_LEN);
        assert!(!k.press(Key::Char('y')).0);
    }

    #[test]
    fn every_column_of_every_row_is_reachable() {
        let caps = caps();
        for row in 0..=CHARS.len() {
            let covered: usize = caps.iter().filter(|c| c.row == row).map(|c| c.width).sum();
            assert_eq!(covered, 10, "row {row}");
        }
    }
}
