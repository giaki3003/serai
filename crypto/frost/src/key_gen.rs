use std::{marker::PhantomData, collections::HashMap};

use rand_core::{RngCore, CryptoRng};

use group::ff::{Field, PrimeField};

use multiexp::{multiexp_vartime, BatchVerifier};

use crate::{
  curve::Curve,
  FrostError, MultisigParams, MultisigKeys,
  schnorr::{self, SchnorrSignature},
  validate_map
};

#[allow(non_snake_case)]
fn challenge<C: Curve>(context: &str, l: u16, R: &[u8], Am: &[u8]) -> C::F {
  const DST: &'static [u8] = b"FROST Schnorr Proof of Knowledge";

  // Uses hash_msg to get a fixed size value out of the context string
  let mut transcript = C::hash_msg(context.as_bytes());
  transcript.extend(l.to_be_bytes());
  transcript.extend(R);
  transcript.extend(Am);
  C::hash_to_F(DST, &transcript)
}

// Implements steps 1 through 3 of round 1 of FROST DKG. Returns the coefficients, commitments, and
// the serialized commitments to be broadcasted over an authenticated channel to all parties
fn generate_key_r1<R: RngCore + CryptoRng, C: Curve>(
  rng: &mut R,
  params: &MultisigParams,
  context: &str,
) -> (Vec<C::F>, Vec<u8>) {
  let t = usize::from(params.t);
  let mut coefficients = Vec::with_capacity(t);
  let mut commitments = Vec::with_capacity(t);
  let mut serialized = Vec::with_capacity((C::G_len() * t) + C::G_len() + C::F_len());

  for i in 0 .. t {
    // Step 1: Generate t random values to form a polynomial with
    coefficients.push(C::F::random(&mut *rng));
    // Step 3: Generate public commitments
    commitments.push(C::GENERATOR_TABLE * coefficients[i]);
    // Serialize them for publication
    serialized.extend(&C::G_to_bytes(&commitments[i]));
  }

  // Step 2: Provide a proof of knowledge
  let r = C::F::random(rng);
  serialized.extend(
    schnorr::sign::<C>(
      coefficients[0],
      // This could be deterministic as the PoK is a singleton never opened up to cooperative
      // discussion
      // There's no reason to spend the time and effort to make this deterministic besides a
      // general obsession with canonicity and determinism though
      r,
      challenge::<C>(
        context,
        params.i(),
        &C::G_to_bytes(&(C::GENERATOR_TABLE * r)),
        &serialized
      )
    ).serialize()
  );

  // Step 4: Broadcast
  (coefficients, serialized)
}

// Verify the received data from the first round of key generation
fn verify_r1<R: RngCore + CryptoRng, C: Curve>(
  rng: &mut R,
  params: &MultisigParams,
  context: &str,
  our_commitments: Vec<u8>,
  mut serialized: HashMap<u16, Vec<u8>>,
) -> Result<HashMap<u16, Vec<C::G>>, FrostError> {
  validate_map(
    &mut serialized,
    &(1 ..= params.n()).into_iter().collect::<Vec<_>>(),
    (params.i(), our_commitments)
  )?;

  let commitments_len = usize::from(params.t()) * C::G_len();

  let mut commitments = HashMap::new();

  #[allow(non_snake_case)]
  let R_bytes = |l| &serialized[&l][commitments_len .. commitments_len + C::G_len()];
  #[allow(non_snake_case)]
  let R = |l| C::G_from_slice(R_bytes(l)).map_err(|_| FrostError::InvalidProofOfKnowledge(l));
  #[allow(non_snake_case)]
  let Am = |l| &serialized[&l][0 .. commitments_len];

  let s = |l| C::F_from_slice(
    &serialized[&l][commitments_len + C::G_len() ..]
  ).map_err(|_| FrostError::InvalidProofOfKnowledge(l));

  let mut signatures = Vec::with_capacity(usize::from(params.n() - 1));
  for l in 1 ..= params.n() {
    let mut these_commitments = vec![];
    for c in 0 .. usize::from(params.t()) {
      these_commitments.push(
        C::G_from_slice(
          &serialized[&l][(c * C::G_len()) .. ((c + 1) * C::G_len())]
        ).map_err(|_| FrostError::InvalidCommitment(l.try_into().unwrap()))?
      );
    }

    // Don't bother validating our own proof of knowledge
    if l != params.i() {
      // Step 5: Validate each proof of knowledge
      // This is solely the prep step for the latter batch verification
      signatures.push((
        l,
        these_commitments[0],
        challenge::<C>(context, l, R_bytes(l), Am(l)),
        SchnorrSignature::<C> { R: R(l)?, s: s(l)? }
      ));
    }

    commitments.insert(l, these_commitments);
  }

  schnorr::batch_verify(rng, &signatures).map_err(|l| FrostError::InvalidProofOfKnowledge(l))?;

  Ok(commitments)
}

fn polynomial<F: PrimeField>(
  coefficients: &[F],
  l: u16
) -> F {
  let l = F::from(u64::from(l));
  let mut share = F::zero();
  for (idx, coefficient) in coefficients.iter().rev().enumerate() {
    share += coefficient;
    if idx != (coefficients.len() - 1) {
      share *= l;
    }
  }
  share
}

// Implements round 1, step 5 and round 2, step 1 of FROST key generation
// Returns our secret share part, commitments for the next step, and a vector for each
// counterparty to receive
fn generate_key_r2<R: RngCore + CryptoRng, C: Curve>(
  rng: &mut R,
  params: &MultisigParams,
  context: &str,
  coefficients: Vec<C::F>,
  our_commitments: Vec<u8>,
  commitments: HashMap<u16, Vec<u8>>,
) -> Result<(C::F, HashMap<u16, Vec<C::G>>, HashMap<u16, Vec<u8>>), FrostError> {
  let commitments = verify_r1::<R, C>(rng, params, context, our_commitments, commitments)?;

  // Step 1: Generate secret shares for all other parties
  let mut res = HashMap::new();
  for l in 1 ..= params.n() {
    // Don't insert our own shares to the byte buffer which is meant to be sent around
    // An app developer could accidentally send it. Best to keep this black boxed
    if l == params.i() {
      continue;
    }

    res.insert(l, C::F_to_bytes(&polynomial(&coefficients, l)));
  }

  // Calculate our own share
  let share = polynomial(&coefficients, params.i());

  // The secret shares are discarded here, not cleared. While any system which leaves its memory
  // accessible is likely totally lost already, making the distinction meaningless when the key gen
  // system acts as the signer system and therefore actively holds the signing key anyways, it
  // should be overwritten with /dev/urandom in the name of security (which still doesn't meet
  // requirements for secure data deletion yet those requirements expect hardware access which is
  // far past what this library can reasonably counter)
  // TODO: Zero out the coefficients

  Ok((share, commitments, res))
}

/// Finishes round 2 and returns both the secret share and the serialized public key.
/// This key is not usable until all parties confirm they have completed the protocol without
/// issue, yet simply confirming protocol completion without issue is enough to confirm the same
/// key was generated as long as a lack of duplicated commitments was also confirmed when they were
/// broadcasted initially
fn complete_r2<R: RngCore + CryptoRng, C: Curve>(
  rng: &mut R,
  params: MultisigParams,
  mut secret_share: C::F,
  commitments: HashMap<u16, Vec<C::G>>,
  // Vec to preserve ownership
  mut serialized: HashMap<u16, Vec<u8>>,
) -> Result<MultisigKeys<C>, FrostError> {
  validate_map(
    &mut serialized,
    &(1 ..= params.n()).into_iter().collect::<Vec<_>>(),
    (params.i(), C::F_to_bytes(&secret_share))
  )?;

  // Step 2. Verify each share
  let mut shares = HashMap::new();
  for (l, share) in serialized {
    shares.insert(l, C::F_from_slice(&share).map_err(|_| FrostError::InvalidShare(l))?);
  }

  // Calculate the exponent for a given participant and apply it to a series of commitments
  // Initially used with the actual commitments to verify the secret share, later used with stripes
  // to generate the verification shares
  let exponential = |i: u16, values: &[_]| {
    let i = C::F::from(i.into());
    let mut res = Vec::with_capacity(params.t().into());
    (0 .. usize::from(params.t())).into_iter().fold(
      C::F::one(),
      |exp, l| {
        res.push((exp, values[l]));
        exp * i
      }
    );
    res
  };

  let mut batch = BatchVerifier::new(shares.len(), C::LITTLE_ENDIAN);
  for (l, share) in &shares {
    if *l == params.i() {
      continue;
    }

    secret_share += share;

    // This can be insecurely linearized from n * t to just n using the below sums for a given
    // stripe. Doing so uses naive addition which is subject to malleability. The only way to
    // ensure that malleability isn't present is to use this n * t algorithm, which runs
    // per sender and not as an aggregate of all senders, which also enables blame
    let mut values = exponential(params.i, &commitments[l]);
    values.push((-*share, C::GENERATOR));
    batch.queue(rng, *l, values);
  }
  batch.verify_with_vartime_blame().map_err(|l| FrostError::InvalidCommitment(l))?;

  // Stripe commitments per t and sum them in advance. Calculating verification shares relies on
  // these sums so preprocessing them is a massive speedup
  // If these weren't just sums, yet the tables used in multiexp, this would be further optimized
  // As of right now, each multiexp will regenerate them
  let mut stripes = Vec::with_capacity(usize::from(params.t()));
  for t in 0 .. usize::from(params.t()) {
    stripes.push(commitments.values().map(|commitments| commitments[t]).sum());
  }

  // Calculate each user's verification share
  let mut verification_shares = HashMap::new();
  for i in 1 ..= params.n() {
    verification_shares.insert(i, multiexp_vartime(&exponential(i, &stripes), C::LITTLE_ENDIAN));
  }
  debug_assert_eq!(C::GENERATOR_TABLE * secret_share, verification_shares[&params.i()]);

  // TODO: Clear serialized and shares

  Ok(
    MultisigKeys {
      params,
      secret_share,
      group_key: stripes[0],
      verification_shares,
      offset: None
    }
  )
}

pub struct KeyGenMachine<C: Curve> {
  params: MultisigParams,
  context: String,
  _curve: PhantomData<C>,
}

pub struct SecretShareMachine<C: Curve> {
  params: MultisigParams,
  context: String,
  coefficients: Vec<C::F>,
  our_commitments: Vec<u8>,
}

pub struct KeyMachine<C: Curve> {
  params: MultisigParams,
  secret: C::F,
  commitments: HashMap<u16, Vec<C::G>>,
}

impl<C: Curve> KeyGenMachine<C> {
  /// Creates a new machine to generate a key for the specified curve in the specified multisig
  // The context string must be unique among multisigs
  pub fn new(params: MultisigParams, context: String) -> KeyGenMachine<C> {
    KeyGenMachine { params, context, _curve: PhantomData }
  }

  /// Start generating a key according to the FROST DKG spec
  /// Returns a serialized list of commitments to be sent to all parties over an authenticated
  /// channel. If any party submits multiple sets of commitments, they MUST be treated as malicious
  pub fn generate_coefficients<R: RngCore + CryptoRng>(
    self,
    rng: &mut R
  ) -> (SecretShareMachine<C>, Vec<u8>) {
    let (coefficients, serialized) = generate_key_r1::<R, C>(rng, &self.params, &self.context);
    (
      SecretShareMachine {
        params: self.params,
        context: self.context,
        coefficients,
        our_commitments: serialized.clone()
      },
      serialized,
    )
  }
}

impl<C: Curve> SecretShareMachine<C> {
  /// Continue generating a key
  /// Takes in everyone else's commitments, which are expected to be in a Vec where participant
  /// index = Vec index. An empty vector is expected at index 0 to allow for this. An empty vector
  /// is also expected at index i which is locally handled. Returns a byte vector representing a
  /// secret share for each other participant which should be encrypted before sending
  pub fn generate_secret_shares<R: RngCore + CryptoRng>(
    self,
    rng: &mut R,
    commitments: HashMap<u16, Vec<u8>>,
  ) -> Result<(KeyMachine<C>, HashMap<u16, Vec<u8>>), FrostError> {
    let (secret, commitments, shares) = generate_key_r2::<R, C>(
      rng,
      &self.params,
      &self.context,
      self.coefficients,
      self.our_commitments,
      commitments,
    )?;
    Ok((KeyMachine { params: self.params, secret, commitments }, shares))
  }
}

impl<C: Curve> KeyMachine<C> {
  /// Complete key generation
  /// Takes in everyone elses' shares submitted to us as a Vec, expecting participant index =
  /// Vec index with an empty vector at index 0 and index i. Returns a byte vector representing the
  /// group's public key, while setting a valid secret share inside the machine. > t participants
  /// must report completion without issue before this key can be considered usable, yet you should
  /// wait for all participants to report as such
  pub fn complete<R: RngCore + CryptoRng>(
    self,
    rng: &mut R,
    shares: HashMap<u16, Vec<u8>>,
  ) -> Result<MultisigKeys<C>, FrostError> {
    complete_r2(rng, self.params, self.secret, self.commitments, shares)
  }
}
