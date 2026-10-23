//! Pure focus-movement rules, kept free of UI types so they can be unit-tested.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    To(usize),
    Stay,
    /// Moved up out of the first row: focus goes to the tab bar.
    ExitTop,
}

pub fn grid_move(focus: usize, len: usize, cols: usize, dir: Dir) -> Move {
    if len == 0 {
        return if dir == Dir::Up {
            Move::ExitTop
        } else {
            Move::Stay
        };
    }
    let col = focus % cols;
    match dir {
        Dir::Up if focus < cols => Move::ExitTop,
        Dir::Up => Move::To(focus - cols),
        Dir::Down if focus + cols < len => Move::To(focus + cols),
        // The last row can be shorter: jump to its last tile instead of getting stuck.
        Dir::Down if focus / cols < (len - 1) / cols => Move::To(len - 1),
        Dir::Down => Move::Stay,
        Dir::Left if col > 0 => Move::To(focus - 1),
        Dir::Right if col + 1 < cols && focus + 1 < len => Move::To(focus + 1),
        Dir::Left | Dir::Right => Move::Stay,
    }
}

pub fn wrap_index(current: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    (current as isize + delta).rem_euclid(count as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_from_first_row_exits_to_tabs() {
        assert_eq!(grid_move(3, 20, 5, Dir::Up), Move::ExitTop);
        assert_eq!(grid_move(7, 20, 5, Dir::Up), Move::To(2));
    }

    #[test]
    fn down_into_short_last_row_lands_on_last_tile() {
        // 12 tiles in 5 columns: last row holds 10 and 11.
        assert_eq!(grid_move(8, 12, 5, Dir::Down), Move::To(11));
        assert_eq!(grid_move(1, 12, 5, Dir::Down), Move::To(6));
        assert_eq!(grid_move(11, 12, 5, Dir::Down), Move::Stay);
    }

    #[test]
    fn left_and_right_stop_at_row_edges() {
        assert_eq!(grid_move(5, 20, 5, Dir::Left), Move::Stay);
        assert_eq!(grid_move(4, 20, 5, Dir::Right), Move::Stay);
        assert_eq!(grid_move(11, 12, 5, Dir::Right), Move::Stay);
        assert_eq!(grid_move(6, 20, 5, Dir::Left), Move::To(5));
    }

    #[test]
    fn empty_grid_only_allows_leaving_upward() {
        assert_eq!(grid_move(0, 0, 5, Dir::Up), Move::ExitTop);
        assert_eq!(grid_move(0, 0, 5, Dir::Down), Move::Stay);
    }

    #[test]
    fn tab_index_wraps_both_ways() {
        assert_eq!(wrap_index(0, 5, -1), 4);
        assert_eq!(wrap_index(4, 5, 1), 0);
        assert_eq!(wrap_index(2, 0, 1), 0);
    }
}
