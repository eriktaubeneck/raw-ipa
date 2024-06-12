use std::borrow::Borrow;

use crate::{
    ff::PrimeField,
    protocol::ipa_prf::malicious_security::lagrange::{
        CanonicalLagrangeDenominator, LagrangeTable,
    },
};

#[allow(non_upper_case_globals)]
pub struct ProofVerifier<F: PrimeField> {
    zero_share: F,
}

///
/// Distributed Zero Knowledge Proofs algorithm drawn from
/// `https://eprint.iacr.org/2023/909.pdf`
///
#[allow(non_upper_case_globals)]
impl<F> ProofVerifier<F>
where
    F: PrimeField,
{
    /// This function outputs an `out_share` and a `ProofVerifier` that consists of a `zero_share`.
    pub fn verify_proof<const λ: usize, const P: usize>(
        out_share: F,
        zkp: &[F; P],
        r: F,
    ) -> (F, Self) {
        let denominator_g = CanonicalLagrangeDenominator::<F, P>::new();
        let lagrange_table_g = LagrangeTable::<F, P, 1>::new(&denominator_g, &r);
        let g_r_share = lagrange_table_g.eval(zkp)[0];
        let sum_share = (0..λ).fold(F::ZERO, |acc, i| acc + zkp[i]);

        // Reveal `b_share` to one another to reconstruct `b` and check if `b = 0`. If the check doesn't pass, abort.
        let zero_share = sum_share - out_share;

        (g_r_share, Self { zero_share })
    }

    /// This function compresses the `u_or_v` values.
    pub fn recurse_u_or_v<J, B, const λ: usize>(u_or_v_iterator: J, r: F) -> Vec<[F; λ]>
    where
        J: Iterator<Item = B>,
        B: Borrow<[F; λ]>,
    {
        let denominator_p_or_q = CanonicalLagrangeDenominator::<F, λ>::new();
        let lagrange_table_p_or_q_r = LagrangeTable::<F, λ, 1>::new(&denominator_p_or_q, &r);

        let mut new_u_or_v_vec = Vec::<[F; λ]>::new();

        // iter and interpolate at x coordinate r
        let mut index = 0;
        let mut new_u_or_v_chunk = [F::ZERO; λ];
        for polynomial in u_or_v_iterator {
            let value_at_r = lagrange_table_p_or_q_r.eval(polynomial.borrow())[0];
            if index >= λ {
                new_u_or_v_vec.push(new_u_or_v_chunk);
                new_u_or_v_chunk = [F::ZERO; λ];
                index = 0;
            }
            new_u_or_v_chunk[index] = value_at_r;
            index += 1;
        }
        if index > 0 {
            new_u_or_v_vec.push(new_u_or_v_chunk);
        }

        new_u_or_v_vec
    }
}

#[cfg(all(test, unit_test))]
mod test {
    use std::iter;

    use super::ProofVerifier;
    use crate::ff::{Fp31, PrimeField, U128Conversions};

    fn make_chunks<F: PrimeField, const N: usize>(a: &[u128]) -> Vec<[F; N]> {
        a.chunks(N)
            .map(|chunk| <[u128; N]>::try_from(chunk).unwrap().map(F::truncate_from))
            .collect::<Vec<_>>()
    }

    #[test]
    fn sample_proof_u() {
        const U_1: [u128; 32] = [
            0, 30, 0, 16, 0, 1, 0, 15, 0, 0, 0, 16, 0, 30, 0, 16, 29, 1, 1, 15, 0, 0, 1, 15, 2, 30,
            30, 16, 0, 0, 30, 16,
        ];
        const OUT_1: u128 = 27;
        const ZKP_1: [u128; 7] = [0, 0, 13, 17, 11, 25, 7];
        const R_1: u128 = 22;

        const EXPECTED_G_R_1: u128 = 0;
        const EXPECTED_B_1: u128 = 3;

        const U_2: [u128; 8] = [0, 0, 26, 0, 7, 18, 24, 13];
        const ZKP_2: [u128; 7] = [11, 25, 17, 9, 22, 23, 3];
        const R_2: u128 = 17;

        const EXPECTED_G_R_2: u128 = 13;
        const EXPECTED_B_2: u128 = 0;

        const ZKP_3: [u128; 5] = [21, 1, 6, 25, 1];
        const U_3: [u128; 2] = [3, 3];
        const R_3: u128 = 30;
        const P_RANDOM_WEIGHT: u128 = 12;

        const EXPECTED_P_FINAL: u128 = 30;
        const EXPECTED_G_R_FINAL: u128 = 0;

        // uv values in input format
        let u_1 = make_chunks::<_, 4>(&U_1);

        // first iteration
        let zkp_1 = ZKP_1.map(Fp31::truncate_from);

        let (out_share_2, pv_1) = ProofVerifier::verify_proof::<4, 7>(
            Fp31::try_from(OUT_1).unwrap(),
            &zkp_1,
            Fp31::try_from(R_1).unwrap(),
        );
        assert_eq!(out_share_2.as_u128(), EXPECTED_G_R_1);
        assert_eq!(pv_1.zero_share.as_u128(), EXPECTED_B_1);

        let u_or_v_2 = ProofVerifier::recurse_u_or_v(u_1.iter(), Fp31::try_from(R_1).unwrap());
        assert_eq!(u_or_v_2, make_chunks::<Fp31, 4>(&U_2));

        // second iteration
        let zkp_2 = ZKP_2.map(Fp31::truncate_from);

        let (out_share_3, pv_2) =
            ProofVerifier::verify_proof::<4, 7>(out_share_2, &zkp_2, Fp31::try_from(R_2).unwrap());

        assert_eq!(out_share_3.as_u128(), EXPECTED_G_R_2);
        assert_eq!(pv_2.zero_share.as_u128(), EXPECTED_B_2);

        let u_or_v_3_temp =
            ProofVerifier::recurse_u_or_v(u_or_v_2.iter(), Fp31::try_from(R_2).unwrap());

        // final proof trim from U4 to U2
        let u_or_v_3 = [
            Fp31::try_from(P_RANDOM_WEIGHT).unwrap(),
            u_or_v_3_temp[0][0],
            u_or_v_3_temp[0][1],
        ];

        assert_eq!([u_or_v_3[1], u_or_v_3[2]], make_chunks::<Fp31, 2>(&U_3)[0]);

        // final iteration
        let zkp_3 = ZKP_3.map(Fp31::truncate_from);

        let (out_share_4, _) =
            ProofVerifier::verify_proof::<3, 5>(out_share_3, &zkp_3, Fp31::try_from(R_3).unwrap());
        assert_eq!(out_share_4.as_u128(), EXPECTED_G_R_FINAL);

        let p_final =
            ProofVerifier::recurse_u_or_v(iter::once(u_or_v_3), Fp31::try_from(R_3).unwrap());

        assert_eq!(p_final[0][0].as_u128(), EXPECTED_P_FINAL);
    }

    #[test]
    fn sample_proof_v() {
        const V_1: [u128; 32] = [
            0, 0, 0, 30, 0, 0, 0, 1, 30, 30, 30, 30, 0, 0, 30, 30, 0, 30, 0, 30, 0, 0, 0, 1, 0, 0,
            1, 1, 0, 0, 1, 1,
        ];
        const OUT_1: u128 = 0;
        const ZKP_1: [u128; 7] = [0, 30, 16, 13, 25, 3, 6];
        const R_1: u128 = 22;

        const EXPECTED_G_R_1: u128 = 10;
        const EXPECTED_B_1: u128 = 28;

        const V_2: [u128; 8] = [10, 21, 30, 28, 15, 21, 3, 3];
        const ZKP_2: [u128; 7] = [1, 12, 29, 30, 7, 7, 3];
        const R_2: u128 = 17;

        const EXPECTED_G_R_2: u128 = 12;
        const EXPECTED_B_2: u128 = 0;

        const ZKP_3: [u128; 5] = [22, 14, 4, 20, 16];
        const V_3: [u128; 2] = [5, 24];
        const R_3: u128 = 30;
        const Q_RANDOM_WEIGHT: u128 = 1;

        const EXPECTED_Q_FINAL: u128 = 12;
        const EXPECTED_G_R_FINAL: u128 = 19;

        // uv values in input format
        let v_1 = make_chunks::<_, 4>(&V_1);

        // first iteration
        let zkp_1 = ZKP_1.map(Fp31::truncate_from);

        let (out_share_2, pv_1) = ProofVerifier::verify_proof::<4, 7>(
            Fp31::try_from(OUT_1).unwrap(),
            &zkp_1,
            Fp31::try_from(R_1).unwrap(),
        );
        assert_eq!(out_share_2.as_u128(), EXPECTED_G_R_1);
        assert_eq!(pv_1.zero_share.as_u128(), EXPECTED_B_1);

        let u_or_v_2 = ProofVerifier::recurse_u_or_v(v_1.iter(), Fp31::try_from(R_1).unwrap());
        assert_eq!(u_or_v_2, make_chunks::<Fp31, 4>(&V_2));

        // second iteration
        let zkp_2 = ZKP_2.map(Fp31::truncate_from);

        let (out_share_3, pv_2) =
            ProofVerifier::verify_proof::<4, 7>(out_share_2, &zkp_2, Fp31::try_from(R_2).unwrap());

        assert_eq!(out_share_3.as_u128(), EXPECTED_G_R_2);
        assert_eq!(pv_2.zero_share.as_u128(), EXPECTED_B_2);

        let u_or_v_3_temp =
            ProofVerifier::recurse_u_or_v(u_or_v_2.iter(), Fp31::try_from(R_2).unwrap());

        // final proof trim from U4 to U2
        let u_or_v_3 = [
            Fp31::try_from(Q_RANDOM_WEIGHT).unwrap(),
            u_or_v_3_temp[0][0],
            u_or_v_3_temp[0][1],
        ];

        assert_eq!([u_or_v_3[1], u_or_v_3[2]], make_chunks::<Fp31, 2>(&V_3)[0]);

        // final iteration
        let zkp_3 = ZKP_3.map(Fp31::truncate_from);

        let (out_share_4, _) =
            ProofVerifier::verify_proof::<3, 5>(out_share_3, &zkp_3, Fp31::try_from(R_3).unwrap());
        assert_eq!(out_share_4.as_u128(), EXPECTED_G_R_FINAL);

        let p_final =
            ProofVerifier::recurse_u_or_v(iter::once(u_or_v_3), Fp31::try_from(R_3).unwrap());

        assert_eq!(p_final[0][0].as_u128(), EXPECTED_Q_FINAL);
    }
}
