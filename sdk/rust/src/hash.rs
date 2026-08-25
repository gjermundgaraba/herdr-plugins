//! Stable content hashing for build-identity checks.

/// 96 bits of FNV-1a over `bytes`, rendered as 24 hex characters.
pub fn fnv(bytes: impl IntoIterator<Item = u8>) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{:024x}", hash & ((1_u128 << 96) - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_content_not_identity() {
        assert_eq!(fnv(b"same".to_vec()), fnv(b"same".to_vec()));
        assert_ne!(fnv(b"same".to_vec()), fnv(b"diff".to_vec()));
        assert_eq!(fnv(b"same".to_vec()).len(), 24);
    }
}
