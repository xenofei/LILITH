//! Spike 12 (plan 4b Task 0): pure helpers and thin Windows wrappers shared by the four
//! binaries. Throwaway code: the plan re-implements what it needs (spikes/README.md).

pub mod pdhnames;
pub mod spill;
pub mod stats;
pub mod win;

use std::path::PathBuf;

/// `--name value` from the command line, if given.
pub fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
    }
    None
}

/// `--name value` as a path; exits with a message when missing.
pub fn path_arg(name: &str) -> PathBuf {
    match arg(name) {
        Some(v) => PathBuf::from(v),
        None => {
            eprintln!("missing {name} <path>");
            std::process::exit(2);
        }
    }
}

/// `--name value` as a number, or `default`.
pub fn num_arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    arg(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The first argument (the subcommand), or `""`.
pub fn subcommand() -> String {
    std::env::args().nth(1).unwrap_or_default()
}

/// Writes `value` as pretty JSON to `path`, creating its folder.
pub fn write_json(path: &std::path::Path, value: &serde_json::Value) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create the output folder");
    }
    let text = serde_json::to_string_pretty(value).expect("serialise");
    std::fs::write(path, text).expect("write the output");
}

/// Milliseconds since the Unix epoch (UTC).
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A small deterministic PRNG (xorshift64*), for generated inputs and data.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
