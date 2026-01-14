//! vsock protocol for host-guest communication.
//!
//! This module re-exports types from smolvm-protocol and adds host-specific
//! functionality like authentication tokens.

// Re-export everything from the shared protocol crate
pub use smolvm_protocol::*;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Authentication token (256-bit random).
#[derive(Clone)]
pub struct AuthToken([u8; 32]);

impl AuthToken {
    /// Generate a new random authentication token.
    ///
    /// Note: This uses a simple PRNG seeded from system time for development.
    /// Phase 1 will use cryptographically secure randomness.
    pub fn generate() -> Self {
        // Counter to ensure uniqueness even when called in same nanosecond
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut bytes = [0u8; 32];
        let time_seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

        // Combine time and counter for seed
        let seed = (time_seed as u64) ^ (counter.wrapping_mul(0x9e3779b97f4a7c15));

        // Simple PRNG (LCG) - NOT cryptographically secure
        // TODO: Use getrandom crate in Phase 1
        let mut state = seed;
        for byte in &mut bytes {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }

        Self(bytes)
    }

    /// Create a token from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Encode as base64.
    pub fn to_base64(&self) -> String {
        base64_encode(&self.0)
    }

    /// Decode from base64.
    pub fn from_base64(s: &str) -> Option<Self> {
        base64_decode(s).map(Self)
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthToken([redacted])")
    }
}

// Simple base64 implementation (no external dependency)
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(s: &str) -> Option<[u8; 32]> {
    const DECODE: [i8; 128] = [
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 62, -1, -1,
        -1, 63, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, -1, -1, -1, -1, -1, -1, -1, 0, 1, 2, 3, 4,
        5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, -1, -1, -1,
        -1, -1, -1, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
        46, 47, 48, 49, 50, 51, -1, -1, -1, -1, -1,
    ];

    let s = s.trim_end_matches('=');
    let mut result = [0u8; 32];
    let mut idx = 0;

    for chunk in s.as_bytes().chunks(4) {
        if chunk.len() < 2 || idx >= 32 {
            break;
        }

        let b0 = DECODE.get(chunk[0] as usize).copied().unwrap_or(-1);
        let b1 = DECODE.get(chunk[1] as usize).copied().unwrap_or(-1);
        let b2 = chunk
            .get(2)
            .and_then(|&c| DECODE.get(c as usize))
            .copied()
            .unwrap_or(0);
        let b3 = chunk
            .get(3)
            .and_then(|&c| DECODE.get(c as usize))
            .copied()
            .unwrap_or(0);

        if b0 < 0 || b1 < 0 {
            return None;
        }

        if idx < 32 {
            result[idx] = ((b0 << 2) | (b1 >> 4)) as u8;
            idx += 1;
        }
        if idx < 32 && chunk.len() > 2 {
            result[idx] = ((b1 << 4) | (b2 >> 2)) as u8;
            idx += 1;
        }
        if idx < 32 && chunk.len() > 3 {
            result[idx] = ((b2 << 6) | b3) as u8;
            idx += 1;
        }
    }

    if idx == 32 {
        Some(result)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_token_base64_roundtrip() {
        // Tests custom base64 implementation (no external dependency)
        let token = AuthToken::generate();
        let encoded = token.to_base64();
        let decoded = AuthToken::from_base64(&encoded).expect("decode failed");
        assert_eq!(token.as_bytes(), decoded.as_bytes());
    }
}
