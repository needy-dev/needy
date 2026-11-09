//! Values derived from a session seed.

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
