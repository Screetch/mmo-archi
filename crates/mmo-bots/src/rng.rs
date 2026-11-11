//! A small generator of numbers that a seed fixes, so a run of the bots can repeat.

/// The generator splitmix64.
pub(crate) struct Rng(u64);

impl Rng {
  pub(crate) fn new(seed: u64) -> Self {
    Self(seed)
  }

  /// A number from 0 up to `bound`, which is more than 0.
  pub(crate) fn below(&mut self, bound: u32) -> u32 {
    self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut mixed = self.0;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    u32::try_from((mixed ^ (mixed >> 31)) >> 32).unwrap_or_default() % bound
  }
}

#[cfg(test)]
mod tests {
  use super::Rng;

  #[test]
  fn a_seed_gives_the_same_numbers_each_time_and_each_stays_below_its_bound() {
    let draw = |seed| {
      let mut rng = Rng::new(seed);
      [rng.below(9), rng.below(9), rng.below(1000), rng.below(1)]
    };
    assert_eq!(draw(7), draw(7));
    assert_ne!(draw(7), draw(8));
    let mut rng = Rng::new(1);
    assert!((0..1000).all(|_| rng.below(9) < 9));
    assert_eq!(draw(1)[3], 0);
  }
}
