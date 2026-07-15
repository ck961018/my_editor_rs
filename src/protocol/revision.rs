#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

impl Revision {
    pub fn next(&mut self) {
        self.0 = self.0.checked_add(1).expect("revision overflow");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_monotonic() {
        let mut revision = Revision::default();
        revision.next();
        revision.next();
        assert_eq!(revision, Revision(2));
    }
}
