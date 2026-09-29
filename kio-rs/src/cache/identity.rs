pub const IMPLEMENTATION_TAG: &str = "kio-rs";
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const FEATURE_SET: &str = env!("KIO_FEATURE_SET");
pub const COMPILER_CACHE_ID: &str = env!("KIO_COMPILER_CACHE_ID");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_cache_id_is_blake3_hex() {
        assert_eq!(COMPILER_CACHE_ID.len(), 64);
        assert!(COMPILER_CACHE_ID.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn feature_set_names_every_declared_crate_feature() {
        for feature in ["cli", "lsp", "parallel", "prime", "repl", "surface"] {
            assert!(
                FEATURE_SET.contains(&format!("{feature}=")),
                "feature set should name `{feature}`: {FEATURE_SET}"
            );
        }
    }
}
