use md5::Md5;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Algo {
    Md5,
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: Algo,
    pub expected: String,
}

impl Checksum {
    /// Parse a hex digest, guessing the algorithm from its length (32 = MD5, 64 = SHA-256).
    pub fn guess(hex: &str) -> Option<Checksum> {
        let h = hex.trim().to_ascii_lowercase();
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let algo = match h.len() {
            32 => Algo::Md5,
            64 => Algo::Sha256,
            _ => return None,
        };
        Some(Checksum { algo, expected: h })
    }
}

pub fn hash_file(path: &Path, algo: Algo) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    macro_rules! run {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }};
    }
    Ok(match algo {
        Algo::Md5 => run!(Md5::new()),
        Algo::Sha256 => run!(Sha256::new()),
    })
}

pub fn verify(path: &Path, c: &Checksum) -> Result<(), String> {
    let got = hash_file(path, c.algo).map_err(|e| e.to_string())?;
    if got.eq_ignore_ascii_case(c.expected.trim()) {
        Ok(())
    } else {
        Err(format!("checksum mismatch: expected {}, got {}", c.expected, got))
    }
}
