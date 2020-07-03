// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use crate::{errors::InternalPakeError, group::Group};
use digest::{BlockInput, FixedOutput, Reset, Update};
use generic_array::{typenum::U32, ArrayLength, GenericArray};
use hkdf::Hkdf;
use rand_core::{CryptoRng, RngCore};

pub(crate) struct OprfClientBytes<Grp: Group> {
    pub(crate) alpha: Grp,
    pub(crate) blinding_factor: Grp::Scalar,
}

/// The `HkDFDigest` trait specifies the interface required for a parameter of `hkdf::Hkdf`.
///
/// It's a convenience wrapper around [`Blockinput`], [`Update`], [`FixedOutput`], [`Reset`],
/// [`Clone`], and [`Default`] traits.
pub trait HkdfDigest: Update + BlockInput + FixedOutput + Reset + Default + Clone
where
    Self::BlockSize: ArrayLength<u8>,
    Self::OutputSize: ArrayLength<u8>,
{
}

impl<T> HkdfDigest for T
where
    T: Update + BlockInput + FixedOutput + Reset + Default + Clone,
    T::BlockSize: ArrayLength<u8>,
    T::OutputSize: ArrayLength<u8>,
{
}

/// Computes the first step for the multiplicative blinding version of DH-OPRF. This
/// message is sent from the client (who holds the input) to the server (who holds the OPRF key).
/// The client can also pass in an optional "pepper" string to be mixed in with the input through
/// an HKDF computation.
pub(crate) fn generate_oprf1<
    R: RngCore + CryptoRng,
    D: HkdfDigest,
    G: Group<UniformBytesLen = D::OutputSize>,
>(
    input: &[u8],
    pepper: Option<&[u8]>,
    blinding_factor_rng: &mut R,
) -> Result<OprfClientBytes<G>, InternalPakeError> {
    let (hashed_input, _) = Hkdf::<D>::extract(pepper, &input);
    let blinding_factor = G::random_scalar(blinding_factor_rng);
    let alpha = G::hash_to_curve(GenericArray::from_slice(&hashed_input)) * &blinding_factor;
    Ok(OprfClientBytes {
        alpha,
        blinding_factor,
    })
}

/// Computes the second step for the multiplicative blinding version of DH-OPRF. This
/// message is sent from the server (who holds the OPRF key) to the client.
pub(crate) fn generate_oprf2<G: Group>(
    point: G,
    oprf_key: &G::Scalar,
) -> Result<G, InternalPakeError> {
    Ok(point * oprf_key)
}

/// Computes the third step for the multiplicative blinding version of DH-OPRF, in which
/// the client unblinds the server's message.
pub(crate) fn generate_oprf3<G: Group>(
    input: &[u8],
    point: G,
    blinding_factor: &G::Scalar,
) -> Result<GenericArray<u8, U32>, InternalPakeError> {
    let unblinded = point * &G::scalar_invert(&blinding_factor);
    let ikm: Vec<u8> = [&unblinded.to_arr()[..], input].concat();
    let (prk, _) = Hkdf::<sha2::Sha256>::extract(None, &ikm);
    Ok(prk)
}

// Tests
// =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::Group;
    use curve25519_dalek::ristretto::RistrettoPoint;
    use generic_array::{arr, GenericArray};
    use hkdf::Hkdf;
    use rand_core::OsRng;
    use sha2::{Digest, Sha256, Sha512};

    fn prf(
        input: &[u8],
        oprf_key: &[u8; 32],
    ) -> GenericArray<u8, <RistrettoPoint as Group>::ElemLen> {
        let (hashed_input, _) = Hkdf::<Sha512>::extract(None, &input);
        let point = RistrettoPoint::hash_to_curve(GenericArray::from_slice(&hashed_input));
        let scalar =
            RistrettoPoint::from_scalar_slice(GenericArray::from_slice(&oprf_key[..])).unwrap();
        let res = point * scalar;
        let ikm: Vec<u8> = [&res.to_arr()[..], &input].concat();

        let (prk, _) = Hkdf::<Sha256>::extract(None, &ikm);
        prk
    }

    #[test]
    fn oprf_retrieval() -> Result<(), InternalPakeError> {
        let input = b"hunter2";
        let mut rng = OsRng;
        let OprfClientBytes {
            alpha,
            blinding_factor,
        } = generate_oprf1::<_, Sha512, RistrettoPoint>(&input[..], None, &mut rng)?;
        let salt_bytes = arr![
            u8; 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32,
        ];
        let salt = RistrettoPoint::from_scalar_slice(&salt_bytes)?;
        let beta = generate_oprf2::<RistrettoPoint>(alpha, &salt)?;
        let res = generate_oprf3::<RistrettoPoint>(input, beta, &blinding_factor)?;
        let res2 = prf(&input[..], &salt.as_bytes());
        assert_eq!(res, res2);
        Ok(())
    }

    #[test]
    fn oprf_inversion_unsalted() {
        let mut rng = OsRng;
        let mut input = vec![0u8; 64];
        rng.fill_bytes(&mut input);
        let OprfClientBytes {
            alpha,
            blinding_factor,
        } = generate_oprf1::<_, Sha512, RistrettoPoint>(&input, None, &mut rng).unwrap();
        let res = generate_oprf3::<RistrettoPoint>(&input, alpha, &blinding_factor).unwrap();

        let (hashed_input, _) = Hkdf::<Sha512>::extract(None, &input);

        // This is because RistrettoPoint is on an obsolete sha2 version
        let mut bits = [0u8; 64];
        let mut hasher = sha2::Sha512::new();
        Digest::update(&mut hasher, &hashed_input[..]);
        bits.copy_from_slice(&hasher.finalize());

        let point = RistrettoPoint::from_uniform_bytes(&bits);
        let mut ikm: Vec<u8> = Vec::new();
        ikm.extend_from_slice(&point.to_arr());
        ikm.extend_from_slice(&input);
        let (prk, _) = Hkdf::<Sha256>::extract(None, &ikm);

        assert_eq!(res, prk);
    }
}
