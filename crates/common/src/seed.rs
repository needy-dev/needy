//! Values derived from a run seed.

/// splitmix64: a small mixer for deriving values from a seed. It is kept here
/// rather than taken from `rand` because its output must never change between
/// versions; replays depend on it.
pub fn mix(seed: u64, salt: u64) -> u64 {
  let mut z = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
  z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// Derives a stable 64-bit seed from an arbitrary string label (for example a
/// profile's `actor.seed: "worker-1"`), using 64-bit FNV-1a followed by [`mix`].
/// Kept here rather than using `std::collections::hash_map::DefaultHasher`
/// because its output must never change between Rust or needy versions.
// Reference: https://datatracker.ietf.org/doc/html/draft-eastlake-fnv-17#section-2
pub fn from_str(text: &str) -> u64 {
  const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
  const PRIME: u64 = 0x0000_0100_0000_01b3;
  let mut hash = OFFSET_BASIS;
  for byte in text.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(PRIME);
  }
  mix(hash, 0)
}

/// Derives a 32-character lowercase hexadecimal Linux `/etc/machine-id` value
/// from `seed`.
// Reference: https://www.freedesktop.org/software/systemd/man/latest/machine-id.html
pub fn machine_id(seed: u64) -> String {
  let a = mix(seed, 12);
  let b = mix(seed, 13);
  format!("{a:016x}{b:016x}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_str_is_deterministic_and_distinguishes_labels() {
    assert_eq!(from_str("worker-1"), from_str("worker-1"));
    assert_ne!(from_str("worker-1"), from_str("worker-2"));
  }

  #[test]
  fn machine_id_is_32_lowercase_hex_chars_and_deterministic() {
    let id = machine_id(from_str("worker-1"));
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{id}");
    assert_eq!(id, machine_id(from_str("worker-1")));
    assert_ne!(id, machine_id(from_str("worker-2")));
  }
}
