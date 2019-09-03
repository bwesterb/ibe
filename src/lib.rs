#![no_std]

//! Identity Based Encryption Waters-Naccache scheme on the [BLS12-381 pairing-friendly elliptic curve](https://github.com/zkcrypto/bls12_381).
//!  * Inspired by: [CHARM implementation](https://github.com/JHUISI/charm/blob/dev/charm/schemes/ibenc/ibenc_waters05.py)
//!  * From: "[Secure and Practical Identity-Based Encryption](http://eprint.iacr.org/2005/369.pdf)"
//!  * Published in: IET Information Security, 2007
//!
//! Uses [SHA3-512](https://crates.io/crates/tiny-keccak) for hashing to identities.
//!
//! The structure of the byte serialisation of the various datastructures is not guaranteed
//! to remain constant between releases of this library.
//! All operations in this library are implemented to run in constant time.

mod util;
use crate::util::*;

use arrayref::{array_mut_ref, array_ref, array_refs, mut_array_refs};
use bls12_381::{G1Affine, G2Affine, G2Projective, Gt, Scalar};
use rand::Rng;
use subtle::{Choice, ConditionallySelectable, CtOption};

const HASH_BIT_LEN: usize = 512;
const HASH_BYTE_LEN: usize = HASH_BIT_LEN / 8;

const BITSIZE: usize = 32;
const CHUNKSIZE: usize = BITSIZE / 8;
const CHUNKS: usize = HASH_BYTE_LEN / CHUNKSIZE;

const PARAMETERSIZE: usize = CHUNKS * 96;
const PUBLICKEYSIZE: usize = 2 * 48 + 2 * 96 + PARAMETERSIZE;

#[derive(Default, Clone, Copy, PartialEq, Debug)]
struct Parameters([G2Affine; CHUNKS]);

impl ConditionallySelectable for Parameters {
    fn conditional_select(a: &Self, b: &Self, choice: Choice) -> Self {
        let mut res = [G2Affine::default(); CHUNKS];
        for (i, (ai, bi)) in a.0.iter().zip(b.0.iter()).enumerate() {
            res[i] = G2Affine::conditional_select(&ai, &bi, choice);
        }
        Parameters(res)
    }
}

impl Parameters {
    pub fn to_bytes(&self) -> [u8; PARAMETERSIZE] {
        let mut res = [0u8; PARAMETERSIZE];
        for i in 0..CHUNKS {
            *array_mut_ref![&mut res, i * 96, 96] = self.0[i].to_compressed();
        }
        res
    }

    pub fn from_bytes(bytes: &[u8; PARAMETERSIZE]) -> CtOption<Self> {
        let mut res = [G2Affine::default(); CHUNKS];
        let mut is_some = Choice::from(1u8);
        for i in 0..CHUNKS {
            is_some &= G2Affine::from_compressed(array_ref![bytes, i * 96, 96])
                .map(|s| {
                    res[i] = s;
                })
                .is_some();
        }
        CtOption::new(Parameters(res), is_some)
    }
}

/// Public key parameters generated by the PKG used to encrypt messages.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PublicKey {
    g: G1Affine,
    g1: G1Affine,
    g2: G2Affine,
    uprime: G2Affine,
    u: Parameters,
}

impl PublicKey {
    pub fn to_bytes(&self) -> [u8; PUBLICKEYSIZE] {
        let mut res = [0u8; PUBLICKEYSIZE];
        let (g, g1, g2, uprime, u) = mut_array_refs![&mut res, 48, 48, 96, 96, PARAMETERSIZE];
        *g = self.g.to_compressed();
        *g1 = self.g1.to_compressed();
        *g2 = self.g2.to_compressed();
        *uprime = self.uprime.to_compressed();
        *u = self.u.to_bytes();
        res
    }

    pub fn from_bytes(bytes: &[u8; PUBLICKEYSIZE]) -> CtOption<Self> {
        let (g, g1, g2, uprime, u) = array_refs![bytes, 48, 48, 96, 96, PARAMETERSIZE];

        let g = G1Affine::from_compressed(g);
        let g1 = G1Affine::from_compressed(g1);
        let g2 = G2Affine::from_compressed(g2);
        let uprime = G2Affine::from_compressed(uprime);
        let u = Parameters::from_bytes(u);

        g.and_then(|g| {
            g1.and_then(|g1| {
                g2.and_then(|g2| {
                    uprime.and_then(|uprime| {
                        u.map(|u| PublicKey {
                            g,
                            g1,
                            g2,
                            uprime,
                            u,
                        })
                    })
                })
            })
        })
    }
}

/// Field parameters for an identity.
///
/// Effectively a hash of an identity, mapped to the curve field.
/// Together with the public key parameters generated by the PKG forms the user public key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Identity([Scalar; CHUNKS]);

/// Secret key parameter generated by the PKG used to extract user secret keys.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecretKey {
    g2prime: G2Affine,
}

impl SecretKey {
    pub fn to_bytes(&self) -> [u8; 96] {
        self.g2prime.to_compressed()
    }

    pub fn from_bytes(bytes: &[u8; 96]) -> CtOption<Self> {
        G2Affine::from_compressed(bytes).map(|g2prime| SecretKey { g2prime })
    }
}

/// Points on the paired curves that form the user secret key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UserSecretKey {
    d1: G2Affine,
    d2: G1Affine,
}

impl UserSecretKey {
    pub fn to_bytes(&self) -> [u8; 144] {
        let mut res = [0u8; 144];
        let (d1, d2) = mut_array_refs![&mut res, 96, 48];
        *d1 = self.d1.to_compressed();
        *d2 = self.d2.to_compressed();
        res
    }

    pub fn from_bytes(bytes: &[u8; 144]) -> CtOption<Self> {
        let (d1, d2) = array_refs![bytes, 96, 48];

        let d1 = G2Affine::from_compressed(d1);
        let d2 = G1Affine::from_compressed(d2);

        d1.and_then(|d1| d2.map(|d2| UserSecretKey { d1, d2 }))
    }
}

/// Encrypted message. Can only be decrypted with an user secret key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CipherText {
    c1: Gt,
    c2: G1Affine,
    c3: G2Affine,
}

impl CipherText {
    pub fn to_bytes(&self) -> [u8; 720] {
        let mut res = [0u8; 720];
        let (c1, c2, c3) = mut_array_refs![&mut res, 576, 48, 96];
        *c1 = self.c1.to_uncompressed(); // TODO implement compressed
        *c2 = self.c2.to_compressed();
        *c3 = self.c3.to_compressed();
        res
    }

    pub fn from_bytes(bytes: &[u8; 720]) -> CtOption<Self> {
        let (c1, c2, c3) = array_refs![bytes, 576, 48, 96];

        let c1 = Gt::from_uncompressed(c1);
        let c2 = G1Affine::from_compressed(c2);
        let c3 = G2Affine::from_compressed(c3);

        c1.and_then(|c1| c2.and_then(|c2| c3.map(|c3| CipherText { c1, c2, c3 })))
    }
}

/// A point on the paired curve that can be encrypted and decrypted.
///
/// You can use the byte representation to derive an AES key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Message(Gt);

impl Message {
    /// Generate a random point on the paired curve.
    pub fn generate<R: Rng>(rng: &mut R) -> Self {
        Self(rand_gt(rng))
    }

    pub fn to_bytes(&self) -> [u8; 576] {
        self.0.to_uncompressed()
    }

    pub fn from_bytes(bytes: &[u8; 576]) -> CtOption<Self> {
        Gt::from_uncompressed(bytes).map(|m| Message(m))
    }
}

/// Generate a keypair used by the Private Key Generator (PKG).
pub fn setup<R: Rng>(rng: &mut R) -> (PublicKey, SecretKey) {
    let g: G1Affine = rand_g1(rng).into();

    let alpha = rand_scalar(rng);
    let g1 = (g * alpha).into();

    let g2 = rand_g2(rng).into();
    let uprime = rand_g2(rng).into();

    let mut u = Parameters([G2Affine::default(); CHUNKS]);
    for ui in u.0.iter_mut() {
        *ui = rand_g2(rng).into();
    }

    let pk = PublicKey {
        g,
        g1,
        g2,
        uprime,
        u,
    };

    let g2prime: G2Affine = (g2 * alpha).into();

    let sk = SecretKey { g2prime };

    (pk, sk)
}

/// Extract an user secret key for a given identity.
pub fn extract_usk<R: Rng>(
    pk: &PublicKey,
    sk: &SecretKey,
    v: &Identity,
    rng: &mut R,
) -> UserSecretKey {
    let mut ucoll: G2Projective = pk.uprime.into();
    for (ui, vi) in pk.u.0.iter().zip(&v.0) {
        ucoll += ui * vi;
    }

    let r = rand_scalar(rng);
    let d1 = (sk.g2prime + (ucoll * r)).into();
    let d2 = (pk.g * r).into();

    UserSecretKey { d1, d2 }
}

/// Encrypt a message using the PKG public key and an identity.
pub fn encrypt<R: Rng>(pk: &PublicKey, v: &Identity, m: &Message, rng: &mut R) -> CipherText {
    let t = rand_scalar(rng);

    let mut c3coll: G2Projective = pk.uprime.into();
    for (ui, vi) in pk.u.0.iter().zip(&v.0) {
        c3coll += ui * vi;
    }

    let c1 = bls12_381::pairing(&pk.g1, &pk.g2) * t + m.0;
    let c2 = (pk.g * t).into();
    let c3 = (c3coll * t).into();

    CipherText { c1, c2, c3 }
}

/// Decrypt ciphertext to a message using a user secret key.
pub fn decrypt(usk: &UserSecretKey, c: &CipherText) -> Message {
    let num = bls12_381::pairing(&usk.d2, &c.c3);
    let dem = bls12_381::pairing(&c.c2, &usk.d1);

    let m = c.c1 + num - dem;
    Message(m)
}

impl Identity {
    /// Hash a byte slice to a set of Identity parameters, which acts as a user public key.
    /// Uses sha3-512 internally.
    pub fn derive(b: &[u8]) -> Identity {
        let hash = tiny_keccak::sha3_512(b);

        let mut result = [Scalar::zero(); CHUNKS];
        for i in 0..CHUNKS {
            result[i] = u64::from(u32::from_le_bytes(*array_ref![
                hash,
                i * CHUNKSIZE,
                CHUNKSIZE
            ]))
            .into();
        }

        Identity(result)
    }

    /// Hash a string slice to a set of Identity parameters.
    /// Directly converts characters to UTF-8 byte representation.
    pub fn derive_str(s: &str) -> Identity {
        Self::derive(s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &'static str = "email:w.geraedts@sarif.nl";

    #[allow(dead_code)]
    struct DefaultSubResults {
        kid: Identity,
        m: Message,
        pk: PublicKey,
        sk: SecretKey,
        usk: UserSecretKey,
        c: CipherText,
    }

    fn perform_default() -> DefaultSubResults {
        let mut rng = rand::thread_rng();

        let id = ID.as_bytes();
        let kid = Identity::derive(id);

        let m = Message::generate(&mut rng);

        let (pk, sk) = setup(&mut rng);
        let usk = extract_usk(&pk, &sk, &kid, &mut rng);

        let c = encrypt(&pk, &kid, &m, &mut rng);

        DefaultSubResults {
            kid,
            m,
            pk,
            sk,
            usk,
            c,
        }
    }

    #[test]
    fn eq_encrypt_decrypt() {
        let results = perform_default();
        let m2 = decrypt(&results.usk, &results.c);

        assert_eq!(results.m, m2);
    }

    #[test]
    fn stability_identity() {
        const REFERENCE: &'static [u32; 16] = &[
            224058892, 3543031066, 2100894308, 1450993543, 380724969, 4144530249, 2749396120,
            320408521, 409248772, 2464563459, 877936958, 2596797041, 3979538376, 3505820338,
            590474010, 189115610,
        ];

        let id = ID.as_bytes();
        let kid = Identity::derive(id);

        for (kidi, ri) in kid.0.iter().zip(REFERENCE) {
            let mut buf = [0u8; 32];
            buf[0..4].copy_from_slice(&ri.to_le_bytes());

            assert_eq!(kidi.to_bytes(), buf);
        }
    }

    #[test]
    fn eq_serialize_deserialize() {
        let result = perform_default();

        assert_eq!(result.m, Message::from_bytes(&result.m.to_bytes()).unwrap());
        assert_eq!(
            result.pk,
            PublicKey::from_bytes(&result.pk.to_bytes()).unwrap()
        );
        assert_eq!(
            result.sk,
            SecretKey::from_bytes(&result.sk.to_bytes()).unwrap()
        );
        assert_eq!(
            result.usk,
            UserSecretKey::from_bytes(&result.usk.to_bytes()).unwrap()
        );
        assert_eq!(
            result.c,
            CipherText::from_bytes(&result.c.to_bytes()).unwrap()
        );
    }
}
