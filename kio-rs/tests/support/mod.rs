//! Compiler executable selection shared by subprocess integration tests.

// Expand only the selected binary's Cargo variable, so reduced-feature tests
// do not require a binary outside their feature configuration.
macro_rules! test_binary {
    ("kio") => {
        std::env::var_os("KIO_DEBUG_TEST_KIO_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_kio")))
    };
    ("kio-prime") => {
        std::env::var_os("KIO_DEBUG_TEST_KIO_PRIME_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_kio-prime")))
    };
}

pub(crate) use test_binary;
