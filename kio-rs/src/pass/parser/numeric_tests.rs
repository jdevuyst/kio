use super::parse_signature_file;

#[test]
fn signature_integer_values_preserve_separators_and_canonical_versions() {
    for (digits, expected) in [
        ("10", 10),
        ("00010", 10),
        ("1_0", 10),
        ("4294967295", u32::MAX),
    ] {
        let source = format!("signature app v({digits});");
        let signature = parse_signature_file(&source, None).unwrap();
        assert_eq!(signature.version, expected);
        let rendered = crate::sig::emit_signature_file(&signature);
        assert!(rendered.contains(&format!("signature app v({expected});")));
        assert_eq!(
            parse_signature_file(&rendered, None).unwrap().version,
            expected
        );
    }
    let source =
        "signature app v(1); v(0001) { nonbreaking { add { module api { pub fn run() -> .; } } } }";
    let signature = parse_signature_file(source, None).unwrap();
    assert_eq!(signature.version, 1);
    assert_eq!(signature.versions[0].version, 1);
    let rendered = crate::sig::emit_signature_file(&signature);
    assert_eq!(
        rendered,
        crate::sig::emit_signature_file(&parse_signature_file(&rendered, None).unwrap())
    );
}

#[test]
fn signature_integer_values_preserve_range_and_history_validation() {
    for digits in ["4294967296", "999999999999999999999999999", "-0", "-1"] {
        let source = format!("signature app v({digits});");
        let error = parse_signature_file(&source, None).unwrap_err();
        assert!(
            error.diag().1.contains("out of range"),
            "{source}: {error:?}"
        );
        let span = error.diag().0;
        assert_eq!(&source[span.start as usize..span.end as usize], digits);
    }
    let error = parse_signature_file("signature app v(0);", None).unwrap_err();
    assert!(error.diag().1.contains("N >= 1"), "{error:?}");
    let error = parse_signature_file(
        "signature app v(1); v(1) { nonbreaking { add { module api {} } } } v(01) { nonbreaking { add { module api {} } } }",
        None,
    ).unwrap_err();
    assert!(error.diag().1.contains("duplicate `v(1)`"), "{error:?}");
}

#[test]
fn signature_versions_do_not_admit_floating_point_or_point_free_exponents() {
    let error = parse_signature_file("signature app v(1.0e0);", None).unwrap_err();
    assert!(error.diag().1.contains("expected a version number"));
    for digits in [
        "1e12",
        "42e0",
        "42E+3",
        "42e-0",
        "-4_2E+0_3",
        "0e999999999999999999999999999",
    ] {
        let source = format!("signature app v({digits});");
        let error = parse_signature_file(&source, None).unwrap_err();
        assert!(
            error
                .diag()
                .1
                .contains("exponent notation is not supported for integer literals")
        );
        let span = error.diag().0;
        assert_eq!(&source[span.start as usize..span.end as usize], digits);
    }
}
