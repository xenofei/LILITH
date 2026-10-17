//! Asset version numbers (I5): `major.minor.patch`, each part `0|[1-9]\d*` (ruling Q7: no
//! leading zeros, so a version always round-trips as text).

use std::fmt;
use std::str::FromStr;

/// A sealed or target version. Ordered by major, then minor, then patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

/// Which part of a version a sign-off raises (I5 "Version numbers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bump {
    Major,
    Minor,
    Patch,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// The next version: a major bump resets minor and patch, a minor bump resets patch.
    pub fn bump(self, b: Bump) -> Version {
        match b {
            Bump::Major => Version::new(self.major + 1, 0, 0),
            Bump::Minor => Version::new(self.major, self.minor + 1, 0),
            Bump::Patch => Version::new(self.major, self.minor, self.patch + 1),
        }
    }
}

/// One numeric part, `0|[1-9]\d*`: ASCII digits only (no sign, space or other script), no
/// leading zero, fitting `u64`.
pub(crate) fn part(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) || (b.len() > 1 && b[0] == b'0') {
        return None;
    }
    s.parse().ok()
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Version, String> {
        let bad = || format!("`{s}` is not a version (major.minor.patch)");
        let mut parts = s.split('.');
        let (Some(a), Some(b), Some(c), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        match (part(a), part(b), part(c)) {
            (Some(major), Some(minor), Some(patch)) => Ok(Version::new(major, minor, patch)),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Bump {
    /// The wire value: `major`, `minor` or `patch`.
    pub fn as_str(self) -> &'static str {
        match self {
            Bump::Major => "major",
            Bump::Minor => "minor",
            Bump::Patch => "patch",
        }
    }
}

impl FromStr for Bump {
    type Err = String;

    fn from_str(s: &str) -> Result<Bump, String> {
        match s {
            "major" => Ok(Bump::Major),
            "minor" => Ok(Bump::Minor),
            "patch" => Ok(Bump::Patch),
            other => Err(format!("`{other}` is not a bump (major, minor or patch)")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-031
    fn version_parse_order_and_bump() {
        let v: Version = "1.4.0".parse().unwrap();
        assert_eq!(v, Version::new(1, 4, 0));
        assert_eq!(v.to_string(), "1.4.0");
        assert_eq!(v.bump(Bump::Minor), Version::new(1, 5, 0));
        assert_eq!(v.bump(Bump::Major), Version::new(2, 0, 0));
        assert_eq!(v.bump(Bump::Patch), Version::new(1, 4, 1));
        assert_eq!(
            Version::new(1, 4, 7).bump(Bump::Minor),
            Version::new(1, 5, 0)
        );

        // Ordered numerically, not as text.
        let mut vs: Vec<Version> = ["1.10.0", "1.9.3", "2.0.0", "1.9.10", "0.1.0"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        vs.sort();
        let sorted: Vec<String> = vs.iter().map(Version::to_string).collect();
        assert_eq!(sorted, ["0.1.0", "1.9.3", "1.9.10", "1.10.0", "2.0.0"]);

        for bad in [
            "1.4",
            "v1.4.0",
            "1.4.0.0",
            "",
            "1..0",
            "1.4.0 ",
            " 1.4.0",
            "1.-4.0",
            "1.+4.0",
            "1.4.x",
            "1.٤.0",
            "99999999999999999999.0.0",
            // Ruling Q7: no leading zeros.
            "01.2.3",
            "1.02.3",
            "1.2.00",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} parsed");
        }
        assert_eq!("0.10.0".parse::<Version>(), Ok(Version::new(0, 10, 0)));
        assert_eq!("0.0.0".parse::<Version>(), Ok(Version::new(0, 0, 0)));
        assert_eq!("minor".parse::<Bump>(), Ok(Bump::Minor));
        assert!("Minor".parse::<Bump>().is_err());
    }
}
