//! Pure index arithmetic for moving through rows and tabs.

/// Moves along a row by `delta`, stopping at either end (rows don't wrap, like PS5).
pub fn step(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + delta).clamp(0, len as isize - 1) as usize
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
    fn step_clamps_at_both_ends() {
        assert_eq!(step(0, 5, -1), 0);
        assert_eq!(step(4, 5, 1), 4);
        assert_eq!(step(2, 5, 8), 4);
        assert_eq!(step(3, 5, -8), 0);
        assert_eq!(step(1, 5, 1), 2);
    }

    #[test]
    fn step_in_empty_row_stays_at_zero() {
        assert_eq!(step(0, 0, 1), 0);
    }

    #[test]
    fn tab_index_wraps_both_ways() {
        assert_eq!(wrap_index(0, 5, -1), 4);
        assert_eq!(wrap_index(4, 5, 1), 0);
        assert_eq!(wrap_index(2, 0, 1), 0);
    }
}
