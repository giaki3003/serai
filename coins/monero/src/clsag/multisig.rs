use rand_core::{RngCore, CryptoRng};

use blake2::{digest::Update, Digest, Blake2b512};

use curve25519_dalek::{
  constants::ED25519_BASEPOINT_TABLE,
  scalar::Scalar,
  edwards::EdwardsPoint
};

use dalek_ff_group as dfg;
use group::Group;
use frost::{Curve, FrostError, algorithm::Algorithm, sign::ParamsView};

use monero::util::ringct::{Key, Clsag};

use crate::{
  hash_to_point,
  frost::{MultisigError, Ed25519, DLEqProof},
  clsag::{SignableInput, sign_core, verify}
};

#[allow(non_snake_case)]
#[derive(Clone, Debug)]
struct ClsagSignInterim {
  c: Scalar,
  s: Scalar,

  clsag: Clsag,
  C_out: EdwardsPoint
}

#[allow(non_snake_case)]
#[derive(Clone, Debug)]
pub struct Multisig {
  b: Vec<u8>,
  AH: dfg::EdwardsPoint,

  msg: [u8; 32],
  input: SignableInput,

  interim: Option<ClsagSignInterim>
}

impl Multisig {
  pub fn new(
    msg: [u8; 32],
    input: SignableInput
  ) -> Result<Multisig, MultisigError> {
    Ok(
      Multisig {
        b: vec![],
        AH: dfg::EdwardsPoint::identity(),

        msg,
        input,

        interim: None
      }
    )
  }
}

impl Algorithm<Ed25519> for Multisig {
  type Signature = (Clsag, EdwardsPoint);

  fn context(&self) -> Vec<u8> {
    let mut context = vec![];
    context.extend(&self.msg);
    context.extend(&self.input.context());
    context
  }

  // We arguably don't have to commit to at all thanks to xG and yG being committed to, both of
  // those being proven to have the same scalar as xH and yH, yet it doesn't hurt
  fn addendum_commit_len() -> usize {
    64
  }

  fn preprocess_addendum<R: RngCore + CryptoRng>(
    rng: &mut R,
    view: &ParamsView<Ed25519>,
    nonces: &[dfg::Scalar; 2]
  ) -> Vec<u8> {
    #[allow(non_snake_case)]
    let H = hash_to_point(&view.group_key().0);
    let h0 = nonces[0].0 * H;
    let h1 = nonces[1].0 * H;
    // 32 + 32 + 64 + 64
    let mut serialized = Vec::with_capacity(192);
    serialized.extend(h0.compress().to_bytes());
    serialized.extend(h1.compress().to_bytes());
    serialized.extend(&DLEqProof::prove(rng, &nonces[0].0, &H, &h0).serialize());
    serialized.extend(&DLEqProof::prove(rng, &nonces[1].0, &H, &h1).serialize());
    serialized
  }

  fn process_addendum(
    &mut self,
    _: &ParamsView<Ed25519>,
    l: usize,
    commitments: &[dfg::EdwardsPoint; 2],
    p: &dfg::Scalar,
    serialized: &[u8]
  ) -> Result<(), FrostError> {
    if serialized.len() != 192 {
      // Not an optimal error but...
      Err(FrostError::InvalidCommitmentQuantity(l, 6, serialized.len() / 32))?;
    }

    let alt = &hash_to_point(&self.input.ring[self.input.i][0]);

    let h0 = <Ed25519 as Curve>::G_from_slice(&serialized[0 .. 32]).map_err(|_| FrostError::InvalidCommitment(l))?;
    DLEqProof::deserialize(&serialized[64 .. 128]).ok_or(FrostError::InvalidCommitment(l))?.verify(
      &alt,
      &commitments[0],
      &h0
    ).map_err(|_| FrostError::InvalidCommitment(l))?;

    let h1 = <Ed25519 as Curve>::G_from_slice(&serialized[32 .. 64]).map_err(|_| FrostError::InvalidCommitment(l))?;
    DLEqProof::deserialize(&serialized[128 .. 192]).ok_or(FrostError::InvalidCommitment(l))?.verify(
      &alt,
      &commitments[1],
      &h1
    ).map_err(|_| FrostError::InvalidCommitment(l))?;

    self.b.extend(&l.to_le_bytes());
    self.b.extend(&serialized[0 .. 64]);
    self.AH += h0 + (h1 * p);

    Ok(())
  }

  fn sign_share(
    &mut self,
    view: &ParamsView<Ed25519>,
    nonce_sum: dfg::EdwardsPoint,
    nonce: dfg::Scalar,
    _: &[u8]
  ) -> dfg::Scalar {
    // Use everyone's commitments to derive a random source all signers can agree upon
    // Cannot be manipulated to effect and all signers must, and will, know this
    let mut rand_source = Blake2b512::new()
      .chain("clsag_randomness")
      .chain(&self.b)
      .finalize()
      .as_slice()
      .try_into()
      .unwrap();

    let mask = Scalar::from_bytes_mod_order_wide(&rand_source);
    rand_source = Blake2b512::digest(&rand_source).as_slice().try_into().unwrap();

    #[allow(non_snake_case)]
    let (clsag, c, mu_C, z, mu_P, C_out) = sign_core(
      rand_source,
      &self.msg,
      &self.input,
      mask,
      nonce_sum.0,
      self.AH.0
    );
    self.interim = Some(ClsagSignInterim { c: c * mu_P, s: c * mu_C * z, clsag, C_out });

    let share = dfg::Scalar(nonce.0 - (c * mu_P * view.secret_share().0));

    share
  }

  fn verify(
    &self,
    _: dfg::EdwardsPoint,
    _: dfg::EdwardsPoint,
    sum: dfg::Scalar
  ) -> Option<Self::Signature> {
    let interim = self.interim.as_ref().unwrap();

    let mut clsag = interim.clsag.clone();
    clsag.s[self.input.i] = Key { key: (sum.0 - interim.s).to_bytes() };
    if verify(&clsag, &self.msg, self.input.image, &self.input.ring, interim.C_out) {
      return Some((clsag, interim.C_out));
    }
    return None;
  }

  fn verify_share(
    &self,
    verification_share: dfg::EdwardsPoint,
    nonce: dfg::EdwardsPoint,
    share: dfg::Scalar,
  ) -> bool {
    let interim = self.interim.as_ref().unwrap();
    return (&share.0 * &ED25519_BASEPOINT_TABLE) == (
      nonce.0 - (interim.c * verification_share.0)
    );
  }
}