use core::fmt::Debug;
use std::collections::HashMap;

use thiserror::Error;

use group::ff::{Field, PrimeField};

mod schnorr;

pub mod curve;
use curve::Curve;
pub mod key_gen;
pub mod algorithm;
pub mod sign;

pub mod tests;

/// Parameters for a multisig
// These fields can not be made public as they should be static
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MultisigParams {
  /// Participants needed to sign on behalf of the group
  t: u16,
  /// Amount of participants
  n: u16,
  /// Index of the participant being acted for
  i: u16,
}

impl MultisigParams {
  pub fn new(
    t: u16,
    n: u16,
    i: u16
  ) -> Result<MultisigParams, FrostError> {
    if (t == 0) || (n == 0) {
      Err(FrostError::ZeroParameter(t, n))?;
    }

    // When t == n, this shouldn't be used (MuSig2 and other variants of MuSig exist for a reason),
    // but it's not invalid to do so
    if t > n {
      Err(FrostError::InvalidRequiredQuantity(t, n))?;
    }
    if (i == 0) || (i > n) {
      Err(FrostError::InvalidParticipantIndex(n, i))?;
    }

    Ok(MultisigParams{ t, n, i })
  }

  pub fn t(&self) -> u16 { self.t }
  pub fn n(&self) -> u16 { self.n }
  pub fn i(&self) -> u16 { self.i }
}

#[derive(Clone, Error, Debug)]
pub enum FrostError {
  #[error("a parameter was 0 (required {0}, participants {1})")]
  ZeroParameter(u16, u16),
  #[error("too many participants (max {1}, got {0})")]
  TooManyParticipants(usize, u16),
  #[error("invalid amount of required participants (max {1}, got {0})")]
  InvalidRequiredQuantity(u16, u16),
  #[error("invalid participant index (0 < index <= {0}, yet index is {1})")]
  InvalidParticipantIndex(u16, u16),

  #[error("invalid signing set ({0})")]
  InvalidSigningSet(String),
  #[error("invalid participant quantity (expected {0}, got {1})")]
  InvalidParticipantQuantity(usize, usize),
  #[error("duplicated participant index ({0})")]
  DuplicatedIndex(usize),
  #[error("missing participant {0}")]
  MissingParticipant(u16),
  #[error("invalid commitment (participant {0})")]
  InvalidCommitment(u16),
  #[error("invalid proof of knowledge (participant {0})")]
  InvalidProofOfKnowledge(u16),
  #[error("invalid share (participant {0})")]
  InvalidShare(u16),

  #[error("internal error ({0})")]
  InternalError(String),
}

// View of keys passable to algorithm implementations
#[derive(Clone)]
pub struct MultisigView<C: Curve> {
  group_key: C::G,
  included: Vec<u16>,
  secret_share: C::F,
  verification_shares: HashMap<u16, C::G>,
}

impl<C: Curve> MultisigView<C> {
  pub fn group_key(&self) -> C::G {
    self.group_key
  }

  pub fn included(&self) -> Vec<u16> {
    self.included.clone()
  }

  pub fn secret_share(&self) -> C::F {
    self.secret_share
  }

  pub fn verification_share(&self, l: u16) -> C::G {
    self.verification_shares[&l]
  }
}

/// Calculate the lagrange coefficient for a signing set
pub fn lagrange<F: PrimeField>(
  i: u16,
  included: &[u16],
) -> F {
  let mut num = F::one();
  let mut denom = F::one();
  for l in included {
    if i == *l {
      continue;
    }

    let share = F::from(u64::try_from(*l).unwrap());
    num *= share;
    denom *= share - F::from(u64::try_from(i).unwrap());
  }

  // Safe as this will only be 0 if we're part of the above loop
  // (which we have an if case to avoid)
  num * denom.invert().unwrap()
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MultisigKeys<C: Curve> {
  /// Multisig Parameters
  params: MultisigParams,

  /// Secret share key
  secret_share: C::F,
  /// Group key
  group_key: C::G,
  /// Verification shares
  verification_shares: HashMap<u16, C::G>,

  /// Offset applied to these keys
  offset: Option<C::F>,
}

impl<C: Curve> MultisigKeys<C> {
  /// Offset the keys by a given scalar to allow for account and privacy schemes
  /// This offset is ephemeral and will not be included when these keys are serialized
  /// Keys offset multiple times will form a new offset of their sum
  /// Not IETF compliant
  pub fn offset(&self, offset: C::F) -> MultisigKeys<C> {
    let mut res = self.clone();
    // Carry any existing offset
    // Enables schemes like Monero's subaddresses which have a per-subaddress offset and then a
    // one-time-key offset
    res.offset = Some(offset + res.offset.unwrap_or(C::F::zero()));
    res.group_key += C::GENERATOR_TABLE * offset;
    res
  }

  pub fn params(&self) -> MultisigParams {
    self.params
  }

  fn secret_share(&self) -> C::F {
    self.secret_share
  }

  pub fn group_key(&self) -> C::G {
    self.group_key
  }

  fn verification_shares(&self) -> HashMap<u16, C::G> {
    self.verification_shares.clone()
  }

  pub fn view(&self, included: &[u16]) -> Result<MultisigView<C>, FrostError> {
    if (included.len() < self.params.t.into()) || (usize::from(self.params.n) < included.len()) {
      Err(FrostError::InvalidSigningSet("invalid amount of participants included".to_string()))?;
    }

    let secret_share = self.secret_share * lagrange::<C::F>(self.params.i, &included);
    let offset = self.offset.unwrap_or(C::F::zero());
    let offset_share = offset * C::F::from(included.len().try_into().unwrap()).invert().unwrap();

    Ok(MultisigView {
      group_key: self.group_key,
      secret_share: secret_share + offset_share,
      verification_shares: self.verification_shares.iter().map(
        |(l, share)| (
          *l,
          (*share * lagrange::<C::F>(*l, &included)) + (C::GENERATOR_TABLE * offset_share)
        )
      ).collect(),
      included: included.to_vec(),
    })
  }

  pub fn serialized_len(n: u16) -> usize {
    8 + C::ID.len() + (3 * 2) + C::F_len() + C::G_len() + (usize::from(n) * C::G_len())
  }

  pub fn serialize(&self) -> Vec<u8> {
    let mut serialized = Vec::with_capacity(MultisigKeys::<C>::serialized_len(self.params.n));
    serialized.extend(u64::try_from(C::ID.len()).unwrap().to_be_bytes());
    serialized.extend(C::ID);
    serialized.extend(&self.params.t.to_be_bytes());
    serialized.extend(&self.params.n.to_be_bytes());
    serialized.extend(&self.params.i.to_be_bytes());
    serialized.extend(&C::F_to_bytes(&self.secret_share));
    serialized.extend(&C::G_to_bytes(&self.group_key));
    for l in 1 ..= self.params.n.into() {
      serialized.extend(&C::G_to_bytes(&self.verification_shares[&l]));
    }
    serialized
  }

  pub fn deserialize(serialized: &[u8]) -> Result<MultisigKeys<C>, FrostError> {
    let mut start = u64::try_from(C::ID.len()).unwrap().to_be_bytes().to_vec();
    start.extend(C::ID);
    let mut cursor = start.len();

    if serialized.len() < (cursor + 4) {
      Err(
        FrostError::InternalError(
          "MultisigKeys serialization is missing its curve/participant quantities".to_string()
        )
      )?;
    }
    if &start != &serialized[.. cursor] {
      Err(
        FrostError::InternalError(
          "curve is distinct between serialization and deserialization".to_string()
        )
      )?;
    }

    let t = u16::from_be_bytes(serialized[cursor .. (cursor + 2)].try_into().unwrap());
    cursor += 2;

    let n = u16::from_be_bytes(serialized[cursor .. (cursor + 2)].try_into().unwrap());
    cursor += 2;
    if serialized.len() != MultisigKeys::<C>::serialized_len(n) {
      Err(FrostError::InternalError("incorrect serialization length".to_string()))?;
    }

    let i = u16::from_be_bytes(serialized[cursor .. (cursor + 2)].try_into().unwrap());
    cursor += 2;

    let secret_share = C::F_from_slice(&serialized[cursor .. (cursor + C::F_len())])
      .map_err(|_| FrostError::InternalError("invalid secret share".to_string()))?;
    cursor += C::F_len();
    let group_key = C::G_from_slice(&serialized[cursor .. (cursor + C::G_len())])
      .map_err(|_| FrostError::InternalError("invalid group key".to_string()))?;
    cursor += C::G_len();

    let mut verification_shares = HashMap::new();
    for l in 1 ..= n {
      verification_shares.insert(
        l,
        C::G_from_slice(&serialized[cursor .. (cursor + C::G_len())])
          .map_err(|_| FrostError::InternalError("invalid verification share".to_string()))?
      );
      cursor += C::G_len();
    }

    Ok(
      MultisigKeys {
        params: MultisigParams::new(t, n, i)
          .map_err(|_| FrostError::InternalError("invalid parameters".to_string()))?,
        secret_share,
        group_key,
        verification_shares,
        offset: None
      }
    )
  }
}

// Validate a map of serialized values to have the expected included participants
pub(crate) fn validate_map<T>(
  map: &mut HashMap<u16, T>,
  included: &[u16],
  ours: (u16, T)
) -> Result<(), FrostError> {
  map.insert(ours.0, ours.1);

  if map.len() != included.len() {
    Err(FrostError::InvalidParticipantQuantity(included.len(), map.len()))?;
  }

  for included in included {
    if !map.contains_key(included) {
      Err(FrostError::MissingParticipant(*included))?;
    }
  }

  Ok(())
}
