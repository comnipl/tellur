pub(crate) trait CorrespondExt: Iterator {
    fn correspond<T, F>(mut self, mut other: T, f: F) -> bool
    where
        Self: Sized,
        T: Iterator,
        F: Fn(Self::Item, T::Item) -> bool,
    {
        loop {
            match (self.next(), other.next()) {
                (Some(a), Some(b)) => {
                    if !f(a, b) {
                        return false;
                    }
                }
                (None, None) => return true,
                _ => return false,
            }
        }
    }
}

impl<I: Iterator> CorrespondExt for I {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correspond_matched_return_true() {
        let a = [1, 2, 3];
        let b = [2, 4, 6];
        assert!(a.iter().correspond(b.iter(), |a, b| a * 2 == *b));
    }

    #[test]
    fn correspond_length_mismatch_return_false() {
        let a = [1, 2, 3];
        let b = [2, 4];
        assert!(!a.iter().correspond(b.iter(), |a, b| a * 2 == *b));
        assert!(!b.iter().correspond(a.iter(), |b, a| a * 2 == *b));
    }

    #[test]
    fn correspond_unmatched_return_false() {
        let a = [1, 2, 3];
        let b = [2, 4, 6];
        assert!(!a.iter().correspond(b.iter(), |a, b| a * 3 == *b));
    }

    #[test]
    fn correspond_empty_return_false() {
        let a = [1, 2, 3];
        let b = [];
        assert!(!a.iter().correspond(b.iter(), |a, b| a * 2 == *b));
    }

    #[test]
    fn correspond_empty_both_return_true() {
        let a = [];
        let b = [];
        assert!(a.iter().correspond(b.iter(), |a, b| a * 2 == *b));
    }

    #[test]
    fn correspond_infinite_iter_evaluation_should_end() {
        let a = 1..;
        let b = [2, 4, 6];
        assert!(!a.clone().correspond(b.iter(), |a, b| a * 2 == *b));
        assert!(!b.iter().correspond(a.clone(), |b, a| a * 2 == *b));
    }
}
