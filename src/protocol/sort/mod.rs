use super::Step;
use std::fmt::Debug;

mod apply;
pub mod bit_permutations;
pub mod reshare;
mod secureapplyinv;
mod shuffle;

#[allow(clippy::module_name_repetitions)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum SortStep {
    BitPermutations,
}

impl Step for SortStep {}

impl AsRef<str> for SortStep {
    fn as_ref(&self) -> &str {
        match self {
            Self::BitPermutations => "permute",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ShuffleStep {
    Step1,
    Step2,
    Step3,
}

impl Step for ShuffleStep {}

impl AsRef<str> for ShuffleStep {
    fn as_ref(&self) -> &str {
        match self {
            Self::Step1 => "shuffle1",
            Self::Step2 => "shuffle2",
            Self::Step3 => "shuffle3",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum ApplyInvStep {
    GenerateRandomPermutation,
    ShuffleInputs,
    ShufflePermutation,
    RevealPermutation,
}

impl Step for ApplyInvStep {}

impl AsRef<str> for ApplyInvStep {
    fn as_ref(&self) -> &str {
        match self {
            Self::GenerateRandomPermutation => "generate_random_permutation",
            Self::ShuffleInputs => "shuffle_inputs",
            Self::ShufflePermutation => "shuffle_permutation",
            Self::RevealPermutation => "reveal_permutation",
        }
    }
}
