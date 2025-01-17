use core::convert::TryInto;

use rand_core::{RngCore, CryptoRng};

use sha2::{digest::Update, Digest, Sha256};

use group::{ff::{Field, PrimeField}, Group, GroupEncoding};

use elliptic_curve::{bigint::{Encoding, U384}, hash2curve::{Expander, ExpandMsg, ExpandMsgXmd}};

use crate::{curve::{CurveError, Curve}, algorithm::Hram};

macro_rules! kp_curve {
  (
    $lib:   ident,
    $Curve: ident,
    $Hram:  ident,

    $ID:      literal,
    $CONTEXT: literal
  ) => {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct $Curve;
    impl Curve for $Curve {
      type F = $lib::Scalar;
      type G = $lib::ProjectivePoint;
      type T = $lib::ProjectivePoint;

      const ID: &'static [u8] = $ID;

      const GENERATOR: Self::G = $lib::ProjectivePoint::GENERATOR;
      const GENERATOR_TABLE: Self::G = $lib::ProjectivePoint::GENERATOR;

      const LITTLE_ENDIAN: bool = false;

      fn random_nonce<R: RngCore + CryptoRng>(secret: Self::F, rng: &mut R) -> Self::F {
        let mut seed = vec![0; 32];
        rng.fill_bytes(&mut seed);
        seed.extend(secret.to_bytes());
        Self::hash_to_F(&[$CONTEXT as &[u8], b"nonce"].concat(), &seed)
      }

      fn hash_msg(msg: &[u8]) -> Vec<u8> {
        (&Sha256::new()
          .chain($CONTEXT)
          .chain(b"digest")
          .chain(msg)
          .finalize()
        ).to_vec()
      }

      fn hash_binding_factor(binding: &[u8]) -> Self::F {
        Self::hash_to_F(&[$CONTEXT as &[u8], b"rho"].concat(), binding)
      }

      fn hash_to_F(dst: &[u8], msg: &[u8]) -> Self::F {
        let mut dst = dst;
        let oversize = Sha256::digest([b"H2C-OVERSIZE-DST-", dst].concat());
        if dst.len() > 255 {
          dst = &oversize;
        }

        // While one of these two libraries does support directly hashing to the Scalar field, the
        // other doesn't. While that's probably an oversight, this is a universally working method
        let mut modulus = vec![0; 16];
        modulus.extend((Self::F::zero() - Self::F::one()).to_bytes());
        let modulus = U384::from_be_slice(&modulus).wrapping_add(&U384::ONE);
        Self::F_from_slice(
          &U384::from_be_slice(&{
            let mut bytes = [0; 48];
            ExpandMsgXmd::<Sha256>::expand_message(
              &[msg],
              dst,
              48
            ).unwrap().fill_bytes(&mut bytes);
            bytes
          }).reduce(&modulus).unwrap().to_be_bytes()[16 ..]
        ).unwrap()
      }

      fn F_len() -> usize {
        32
      }

      fn G_len() -> usize {
        33
      }

      fn F_from_slice(slice: &[u8]) -> Result<Self::F, CurveError> {
        let bytes: [u8; 32] = slice.try_into()
          .map_err(|_| CurveError::InvalidLength(32, slice.len()))?;

        let scalar = Self::F::from_repr(bytes.into());
        if scalar.is_none().into() {
          Err(CurveError::InvalidScalar)?;
        }

        Ok(scalar.unwrap())
      }

      fn G_from_slice(slice: &[u8]) -> Result<Self::G, CurveError> {
        let bytes: [u8; 33] = slice.try_into()
          .map_err(|_| CurveError::InvalidLength(33, slice.len()))?;

        let point = Self::G::from_bytes(&bytes.into());
        if point.is_none().into() || point.unwrap().is_identity().into() {
          Err(CurveError::InvalidPoint)?;
        }

        Ok(point.unwrap())
      }

      fn F_to_bytes(f: &Self::F) -> Vec<u8> {
        f.to_bytes().to_vec()
      }

      fn G_to_bytes(g: &Self::G) -> Vec<u8> {
        g.to_bytes().to_vec()
      }
    }

    #[derive(Clone)]
    pub struct $Hram;
    impl Hram<$Curve> for $Hram {
      #[allow(non_snake_case)]
      fn hram(R: &$lib::ProjectivePoint, A: &$lib::ProjectivePoint, m: &[u8]) -> $lib::Scalar {
        $Curve::hash_to_F(
          &[$CONTEXT as &[u8], b"chal"].concat(),
          &[&$Curve::G_to_bytes(R), &$Curve::G_to_bytes(A), m].concat()
        )
      }
    }
  }
}

#[cfg(feature = "p256")]
kp_curve!(
  p256,
  P256,
  IetfP256Hram,
  b"P-256",
  b"FROST-P256-SHA256-v5"
);

#[cfg(feature = "secp256k1")]
kp_curve!(
  k256,
  Secp256k1,
  NonIetfSecp256k1Hram,
  b"secp256k1",
  b"FROST-secp256k1-SHA256-v5"
);
