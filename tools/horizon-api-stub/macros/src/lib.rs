//! `#[horizon_mod]` stand-in: leaves the impl unchanged.
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn horizon_mod(_: TokenStream, item: TokenStream) -> TokenStream {
    item
}
