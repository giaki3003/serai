use rand_core::{RngCore, CryptoRng};

use group::{ff::Field, Group};

use crate::{Curve, MultisigKeys, tests::key_gen};

// Test generation of FROST keys
fn key_generation<R: RngCore + CryptoRng, C: Curve>(rng: &mut R) {
  // This alone verifies the verification shares and group key are agreed upon as expected
  key_gen::<_, C>(rng);
}

// Test serialization of generated keys
fn keys_serialization<R: RngCore + CryptoRng, C: Curve>(rng: &mut R) {
  for (_, keys) in key_gen::<_, C>(rng) {
    assert_eq!(&MultisigKeys::<C>::deserialize(&keys.serialize()).unwrap(), &*keys);
  }
}

pub fn test_curve<R: RngCore + CryptoRng, C: Curve>(rng: &mut R) {
  // TODO: Test the Curve functions themselves

  // Test successful multiexp, with enough pairs to trigger its variety of algorithms
  // TODO: This should probably be under multiexp
  {
    let mut pairs = Vec::with_capacity(1000);
    let mut sum = C::G::identity();
    for _ in 0 .. 10 {
      for _ in 0 .. 100 {
        pairs.push((C::F::random(&mut *rng), C::GENERATOR * C::F::random(&mut *rng)));
        sum += pairs[pairs.len() - 1].1 * pairs[pairs.len() - 1].0;
      }
      assert_eq!(multiexp::multiexp(&pairs, C::LITTLE_ENDIAN), sum);
      assert_eq!(multiexp::multiexp_vartime(&pairs, C::LITTLE_ENDIAN), sum);
    }
  }

  // Test FROST key generation and serialization of MultisigKeys works as expected
  key_generation::<_, C>(rng);
  keys_serialization::<_, C>(rng);
}
