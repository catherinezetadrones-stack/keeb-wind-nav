//! Hint-label generation.
//!
//! Labels are **prefix-free** so no completed label is a prefix of another —
//! that keeps match/click logic unambiguous and every hint reachable:
//!   - `n <= 26`  → single letters `a`..
//!   - `n  > 26`  → uniform two-letter labels `aa`, `ab`, … (up to 676)
//!
//! (The Python prototype mixed single + double letters, which made e.g. `aa`
//! unreachable once `a` existed. This is a deliberate improvement.)

/// Characters used to build hint labels.
const HINT_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// Return `n` unique, prefix-free labels.
pub fn labels(n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    if n <= HINT_CHARS.len() {
        return HINT_CHARS
            .iter()
            .take(n)
            .map(|&c| (c as char).to_string())
            .collect();
    }

    // Uniform two-letter labels (prefix-free among themselves).
    let mut out = Vec::with_capacity(n);
    'outer: for &a in HINT_CHARS {
        for &b in HINT_CHARS {
            out.push(format!("{}{}", a as char, b as char));
            if out.len() >= n {
                break 'outer;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::labels;

    #[test]
    fn single_letters_when_small() {
        let l = labels(5);
        assert_eq!(l, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn two_letter_when_large() {
        let l = labels(30);
        assert_eq!(l.len(), 30);
        assert_eq!(l[0], "aa");
        assert_eq!(l[1], "ab");
        assert_eq!(l[26], "ba");
    }

    #[test]
    fn prefix_free() {
        // No label is a prefix of any other.
        let l = labels(50);
        for a in &l {
            for b in &l {
                if a != b {
                    assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
                }
            }
        }
    }
}
