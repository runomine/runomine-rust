//! The visitor hash from the contract (`docs/contract.md`, "The visitor hash"):
//!
//! ```text
//! salt    = hex(HMAC-SHA256(key = token, message = "runomine:" + day))
//! visitor = first 16 hex chars of hex(SHA256(salt + "|" + ip + "|" + user_agent + "|" + host))
//! ```

use sha2::{Digest, Sha256};

use crate::util::{normalize_host, utc_day};

/// The runomine visitor hash: 16 hex characters, the same for one person on one site for one UTC day.
///
/// - `token`: the site's server token (`rm_live_…`).
/// - `ip`: the visitor's IP. It is only hashed here, on your server; runomine never receives it.
/// - `host`: the request's hostname; lowercased and stripped of its port here.
/// - `day`: the UTC date, `"YYYY-MM-DD"` (see [`today`]).
///
/// ```
/// let visitor = runomine::visitor_hash(
///     "rm_live_test_0123456789abcdef",
///     "203.0.113.9",
///     "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15",
///     "example.com",
///     "2026-10-13",
/// );
/// assert_eq!(visitor, "5f9e1bebe31e8bf5");
/// ```
pub fn visitor_hash(token: &str, ip: &str, user_agent: &str, host: &str, day: &str) -> String {
    let salt = salt(token, day);
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b"|");
    hasher.update(ip.as_bytes());
    hasher.update(b"|");
    hasher.update(user_agent.as_bytes());
    hasher.update(b"|");
    hasher.update(normalize_host(host).as_bytes());
    let mut hex = to_hex(&hasher.finalize()[..]);
    hex.truncate(16);
    hex
}

/// Today's UTC date, `"YYYY-MM-DD"`, the `day` of [`visitor_hash`].
pub fn today() -> String {
    utc_day(std::time::SystemTime::now())
}

/// `hex(HMAC-SHA256(key = token, message = "runomine:" + day))`.
pub(crate) fn salt(token: &str, day: &str) -> String {
    to_hex(&hmac_sha256(
        token.as_bytes(),
        format!("runomine:{day}").as_bytes(),
    ))
}

/// HMAC-SHA256 (RFC 2104), written out so the crate works with sha2 0.10 and 0.11 alike.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key)[..]);
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(&inner[..]);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize()[..]);
    out
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "rm_live_test_0123456789abcdef";
    const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15";

    #[test]
    fn contract_vector_salt() {
        assert_eq!(
            salt(TOKEN, "2026-10-13"),
            "fd5d804a4a60e713ada2bcef694204ccfece37b96dfff60153236f075014f883"
        );
    }

    #[test]
    fn contract_vector_visitor() {
        assert_eq!(
            visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", "2026-10-13"),
            "5f9e1bebe31e8bf5"
        );
    }

    #[test]
    fn host_is_lowercased_and_stripped_of_its_port() {
        let expected = visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", "2026-10-13");
        assert_eq!(
            visitor_hash(TOKEN, "203.0.113.9", UA, "Example.COM:8443", "2026-10-13"),
            expected
        );
    }

    #[test]
    fn changes_with_the_day_and_the_token() {
        let a = visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", "2026-10-13");
        assert_ne!(
            a,
            visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", "2026-10-14")
        );
        assert_ne!(
            a,
            visitor_hash(
                "rm_live_other",
                "203.0.113.9",
                UA,
                "example.com",
                "2026-10-13"
            )
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_case_2_and_long_keys() {
        // RFC 4231, test case 2.
        assert_eq!(
            to_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // RFC 4231, test case 6 (key longer than the block).
        assert_eq!(
            to_hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
