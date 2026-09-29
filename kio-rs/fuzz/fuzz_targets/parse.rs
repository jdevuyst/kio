#![no_main]
//
// Fuzz target: feed arbitrary UTF-8 byte sequences to the surface
// parser entry point. A panic, hang, or other libFuzzer-detected
// fault names a parser bug — well-formed input must produce a
// Module, ill-formed input must produce an Error, neither should
// crash or loop. Each surviving crash gets promoted to a regression
// case under test-data/goldens/<NN_category>/ keyed by the exit-code
// category the parser eventually settled on for that input.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let _ = kio_lang::pass::parser::parse(source);
    }
});
