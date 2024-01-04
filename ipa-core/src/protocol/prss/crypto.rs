use aes::{
    cipher::{BlockEncrypt, KeyInit},
    Aes256,
};
use generic_array::{ArrayLength, GenericArray};
use hkdf::Hkdf;
use rand::{CryptoRng, RngCore};
use sha2::Sha256;
use typenum::U1;
use x25519_dalek::{EphemeralSecret, PublicKey};

use crate::{
    ff::Field,
    protocol::prss::PrssIndex,
    secret_sharing::{
        replicated::{semi_honest::AdditiveShare as Replicated, ReplicatedSecretSharing},
        SharedValue,
    },
};

/// Trait for random generation from random u128s.
///
/// It was previously assumed that our fields were of order << 2^128, in which case
/// `Field::truncate_from` can be used for this purpose. This trait makes the contract explicit.
pub trait FromRandomU128 {
    /// Generate a random value of `Self` from a uniformly-distributed random u128.
    fn from_random_u128(src: u128) -> Self;
}

impl FromRandomU128 for u128 {
    fn from_random_u128(src: u128) -> Self {
        src
    }
}

/// Trait for random generation.
///
/// The exact semantics of the generation depend on the value being generated, but like
/// `rand::distributions::Standard`, a uniform distribution is typical. When implementing
/// this trait, consider the consequences if the implementation were to be used in
/// an unexpected way. For example, an implementation that draws from a subset of the
/// possible values could be dangerous, if used in an unexpected context where
/// security relies on sampling from the full space.
pub trait FromRandom: Sized {
    type SourceLength: ArrayLength;

    /// Generate a random value of `Self` from `SourceLength` uniformly-distributed u128s.
    fn from_random(src: GenericArray<u128, Self::SourceLength>) -> Self;
}

impl<T: FromRandomU128> FromRandom for T {
    type SourceLength = U1;

    fn from_random(src: GenericArray<u128, U1>) -> Self {
        Self::from_random_u128(src[0])
    }
}

/// Trait for things that can be generated by PRSS.
///
/// We support two kinds of PRSS generation:
///  1. Raw values: In this case, two values are generated, one using the randomness that is shared
///     with the left helper, and one with the randomness that is shared with the right helper.
///     Thus, one of the generated values is known to both us and the left helper, and likewise for
///     the right helper.
///  2. Secret sharings: In this case, a single secret-shared random value is generated. The value
///     returned by `FromPrss` is our share of that sharing. Within `FromPrss`, the randomness shared
///     with the left and right helpers is used to construct the sharing.
///
/// In the first case, `FromPrss` is implemented for a tuple type, while in the second case,
/// `FromPrss` is implemented for a secret-shared type.
pub trait FromPrss: Sized {
    fn from_prss<P: SharedRandomness + ?Sized, I: Into<PrssIndex>>(prss: &P, index: I) -> Self;
}

/// Generate two random values, one that is known to the left helper
/// and one that is known to the right helper.
impl<T: FromRandom> FromPrss for (T, T) {
    fn from_prss<P: SharedRandomness + ?Sized, I: Into<PrssIndex>>(prss: &P, index: I) -> (T, T) {
        let (l, r) = prss.generate_arrays(index);
        (T::from_random(l), T::from_random(r))
    }
}

/// Generate a replicated secret sharing of a random value, which none
/// of the helpers knows. This is an implementation of the functionality 2.1 `F_rand`
/// described on page 5 of the paper:
/// "Efficient Bit-Decomposition and Modulus Conversion Protocols with an Honest Majority"
/// by Ryo Kikuchi, Dai Ikarashi, Takahiro Matsuda, Koki Hamada, and Koji Chida
/// <https://eprint.iacr.org/2018/387.pdf>
impl<T: FromRandom + SharedValue> FromPrss for Replicated<T> {
    fn from_prss<P: SharedRandomness + ?Sized, I: Into<PrssIndex>>(
        prss: &P,
        index: I,
    ) -> Replicated<T> {
        let (l, r) = <(T, T) as FromPrss>::from_prss(prss, index);
        Replicated::new(l, r)
    }
}

pub trait SharedRandomness {
    /// Generate two random values, one that is known to the left helper
    /// and one that is known to the right helper.
    #[must_use]
    fn generate_arrays<I: Into<PrssIndex>, N: ArrayLength>(
        &self,
        index: I,
    ) -> (GenericArray<u128, N>, GenericArray<u128, N>);

    /// Generate two random values, one that is known to the left helper
    /// and one that is known to the right helper.
    #[must_use]
    fn generate_values<I: Into<PrssIndex>>(&self, index: I) -> (u128, u128) {
        let (l, r) = self.generate_arrays::<_, U1>(index);
        (l[0], r[0])
    }

    /// Generate two random field values, one that is known to the left helper
    /// and one that is known to the right helper.
    ///
    /// This alias is provided for compatibility with existing code. New code can just use
    /// `generate`.
    #[must_use]
    fn generate_fields<F: Field, I: Into<PrssIndex>>(&self, index: I) -> (F, F) {
        self.generate(index)
    }

    /// Generate something that implements the `FromPrss` trait.
    ///
    /// Generation by `FromPrss` is described in more detail in the `FromPrss` documentation.
    #[must_use]
    fn generate<T: FromPrss, I: Into<PrssIndex>>(&self, index: I) -> T {
        T::from_prss(self, index)
    }

    /// Generate a non-replicated additive secret sharing of zero.
    ///
    /// This is used for the MAC accumulators for malicious security.
    //
    // Equivalent functionality could be obtained by defining an `Unreplicated<F>` type that
    // implements `FromPrss`.
    #[must_use]
    fn zero<V: SharedValue + FromRandomU128, I: Into<PrssIndex>>(&self, index: I) -> V {
        let (l, r): (V, V) = self.generate(index);
        l - r
    }
}

// The key exchange component of a participant.
pub struct KeyExchange {
    sk: EphemeralSecret,
}

impl KeyExchange {
    pub fn new<R: RngCore + CryptoRng>(r: &mut R) -> Self {
        Self {
            sk: EphemeralSecret::random_from_rng(r),
        }
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from(&self.sk)
    }

    #[must_use]
    pub fn key_exchange(self, pk: &PublicKey) -> GeneratorFactory {
        debug_assert_ne!(pk, &self.public_key(), "self key exchange detected");
        let secret = self.sk.diffie_hellman(pk);
        let kdf = Hkdf::<Sha256>::new(None, secret.as_bytes());
        GeneratorFactory { kdf }
    }
}

/// This intermediate object exists so that multiple generators can be constructed,
/// with each one dedicated to one purpose.
pub struct GeneratorFactory {
    kdf: Hkdf<Sha256>,
}

impl GeneratorFactory {
    /// Create a new generator using the provided context string.
    #[allow(clippy::missing_panics_doc)] // Panic should be impossible.
    #[must_use]
    pub fn generator(&self, context: &[u8]) -> Generator {
        let mut k = aes::cipher::generic_array::GenericArray::default();
        self.kdf.expand(context, &mut k).unwrap();
        Generator {
            cipher: Aes256::new(&k),
        }
    }
}

/// The basic generator.  This generates values based on an arbitrary index.
#[derive(Debug, Clone)]
pub struct Generator {
    cipher: Aes256,
}

impl Generator {
    /// Generate the value at the given index.
    /// This uses the MMO^{\pi} function described in <https://eprint.iacr.org/2019/074>.
    #[must_use]
    pub fn generate(&self, index: u128) -> u128 {
        let mut buf = index.to_le_bytes();
        self.cipher
            .encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
                &mut buf,
            ));

        u128::from_le_bytes(buf) ^ index
    }
}
