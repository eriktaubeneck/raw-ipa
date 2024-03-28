pub mod convert_shares;
pub mod step;

// TODO: wean usage off convert_some_bits.
pub(crate) use convert_shares::convert_some_bits;
pub use convert_shares::{
    convert_bits, convert_selected_bits, BitConversionTriple, LocalBitConverter,
    ToBitConversionTriples,
};
