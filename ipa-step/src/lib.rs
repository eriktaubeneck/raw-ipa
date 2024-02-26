#![allow(clippy::module_name_repetitions)]

#[cfg(any(feature = "descriptive", debug_assertions))]
pub mod descriptive;
#[cfg(feature = "build")]
pub mod gate;
#[cfg(feature = "name")]
pub mod name;

#[cfg(feature = "build")]
pub use gate::build as build_gate;

pub const COMPACT_GATE_INCLUDE_ENV: &str = "COMPACT_GATE_INCLUDE";

/// Defines a unique step of the IPA protocol at a given level of implementation.
///
/// Any stage of the protocol execution will involve multiple steps.  Each of these steps
/// then might involve executing a process that can be broken down into further steps.
/// Ultimately, there will be processes that need to invoke functions on a PRSS or send
/// data to another helper that needs to be uniquely identified.
///
/// Steps are therefore composed into a hierarchy where top-level steps describe major
/// building blocks for a protocol (such as sort shares, convert shares, apply DP, etc...),
/// intermediate processes describe reusable processes (like shuffling), and steps at the
/// lowest level unique identify multiplications.
///
/// Steps are therefore composed into a `UniqueStepIdentifier`, which collects the complete
/// hierarchy of steps at each layer into a unique identifier.
pub trait Step: AsRef<str> {}

// In test code, allow a string (or string reference) to be used as a `Step`.
#[cfg(any(feature = "descriptive", debug_assertions))]
impl Step for String {}

#[cfg(any(feature = "descriptive", debug_assertions))]
impl Step for str {}

/// For gate identifier, this takes a step toward an adjacent gate.
pub trait StepNarrow<S: Step + ?Sized> {
    #[must_use]
    fn narrow(&self, step: &S) -> Self;
}

/// Implementations of `Step` can also implement `compact::Step` to enable the use of
/// `CompactGate` implementations.  The `ipa-step-derive` crate provides a means of
/// automatically implementing this trait.
pub trait CompactStep: Step {
    /// The total number of steps that can be reached from this step.
    const STEP_COUNT: usize;

    /// Get the index an instance of this type.
    #[must_use]
    fn index(&self) -> usize;

    /// Create a string representation for the step at index `i`.
    #[must_use]
    fn step_string(i: usize) -> String;

    /// For a given step index, `i`, indicate the narrowing type.
    /// This only applies to step indices that have a child;
    /// a step index that does not have a child will return `None`.
    #[must_use]
    fn step_narrow_type(_i: usize) -> Option<&'static str> {
        None
    }
}

/// A `Gate` implementation is a marker trait for a type that can be used to identify
/// gates in a protocol.  It can be mapped to and from strings and has a default value.
/// In most cases, implementations will also implement `StepNarrow` for different types,
/// but this is not strictly required.
pub trait Gate: Default + AsRef<str> + for<'a> From<&'a str> {}
