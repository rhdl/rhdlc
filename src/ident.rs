use syn::Ident;

/// TODO: check all idents at the very beginning
/// https://github.com/rust-lang/rust/blob/5ef299eb9805b4c86b227b718b39084e8bf24454/src/librustc_span/symbol.rs#L1592
pub fn can_be_raw(ident: &Ident) -> bool {
    ident != "_" && ident != "" && !is_path_segment_keyword(ident)
}

/// https://github.com/rust-lang/rust/blob/5ef299eb9805b4c86b227b718b39084e8bf24454/src/librustc_span/symbol.rs#L1577
pub fn is_path_segment_keyword(ident: &Ident) -> bool {
    ident == "super" || ident == "self" || ident == "Self" || ident == "crate"
}