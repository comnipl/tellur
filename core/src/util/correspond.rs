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

impl <I: Iterator> CorrespondExt for I {}