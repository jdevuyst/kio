#![no_main]
//
// Fuzz target: feed arbitrary UTF-8 byte sequences to the lexer.
// Same crash-or-error invariant as the parser target — every input
// must lex to either a token vector or an Error, never panic. Lexer
// crashes are a tighter contract than parser crashes because the
// corpus invariant is "any input that lexes successfully produces
// output" (see test-data/highlight-corpus/'s comment on this).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let _ = kio_lang::pass::lexer::lex(source);
    }
});
