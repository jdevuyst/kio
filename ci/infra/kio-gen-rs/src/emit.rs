//! Render a generated program to package source files and write a
//! case directory consumed by `tests/run-tests.sh`.
//!
//! Case-directory layout:
//!
//! ```text
//! <out_root>/<NN_category>/<case_name>/
//!   expected.stderr.ignore   (empty marker — no portable stderr oracle)
//!   expected.exit            (single line: integer)
//!   expected.stdout          (empty)
//!   run.args                 (generator-selected exact runner protocol)
//!   workdir/
//!     prog.pkg.kio       (`package prog;`, leading `build { cache +
//!                      target js + target ts + target python +
//!                      target java + target rust }`, then a
//!                      `bridge { prog }` glob block)
//!     prog.kio           (module prog; `host type` declarations for
//!                      every base role type, polymorphic helpers +
//!                      deftypes + fn gen(...))
//!     *.kio  (elaborator support files when surface
//!                      elaborator forms may be emitted)
//! ```
//!
//! Each module file sits at the path its `module <path>;`
//! declaration implies relative to the package root (`workdir/`), per the
//! module-path / filesystem coherence rule in `specs/package.md`
//! § Module-name rules.

use std::fs;
use std::io;
use std::path::Path;

use kio_gen::ast::{
    Program, UserElaboratorCapture, UserElaboratorImplementation, UserElaboratorSpec,
    UserElaboratorTemplate,
};
use kio_gen::render;

pub const GENERATED_CORE_MAIN_PROTOCOL: &str = "generated-core-main";
pub const GENERATED_SURFACE_MAIN_PROTOCOL: &str = "generated-surface-main";

pub const fn runner_protocol(surface_mode: bool) -> &'static str {
    if surface_mode {
        GENERATED_SURFACE_MAIN_PROTOCOL
    } else {
        GENERATED_CORE_MAIN_PROTOCOL
    }
}

// The package file carries a leading `build { ... }` block declaring
// the `js`, `ts`, `python`, `java`, and `rust` targets so the generative harness can
// drive each generated case through `kio build $KIO_TARGET` under any of those
// impls. (The `ts` target emits the JS backend's `<pkg>.js` byte-identical
// plus a `<pkg>.d.ts` skin.) `cache` is mandatory per
// `specs/package.md` § Build target files; a fixed in-package path
// suffices here, the actual runner build cache lives under the harness-
// supplied `KIO_TEST_RUNNER_BUILD_CACHE_DIR`, not this per-package cache
// directory. Target ids are bare identifiers.
const PACKAGE_FILE_KIO_PREFIX: &str = "\
package prog;

build {
  cache \"out/.kio-cache/\";

  target js {
    out \"out/js/\"
  };

  target ts {
    out \"out/ts/\"
  };

  target python {
    out \"out/python/\"
  };

  target java {
    out \"out/java/\"
  };

  target rust {
    out \"out/rust/\"
  }
}
";

// The base host is now declared as module-level `host type`
// items in the root module `prog.kio` (post host/bridge redesign — the
// package-level `env {}` block and the identity `bridge prog { … }`
// adaptation block are both retired). The package file selects the
// bridged modules with a `bridge` glob list instead.
const HOST_ENV_KIO: &str = "\
host type I8     role(i8);
host type I16    role(i16);
host type I32    role(i32);
host type I64    role(i64);
host type I128   role(i128);
host type U8     role(u8);
host type U16    role(u16);
host type U32    role(u32);
host type U64    role(u64);
host type U128   role(u128);
host type F32    role(f32);
host type F64    role(f64);
host type String role(str);
host type Bool   role(bool);
";

/// Render a package-level `bridge` block with semicolons between module roots.
pub fn bridge_glob_block<'a>(roots: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::from("bridge {\n");
    let mut roots = roots.into_iter().peekable();
    while let Some(root) = roots.next() {
        out.push_str(&format!("  {root}"));
        if roots.peek().is_some() {
            out.push(';');
        }
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

/// True when the package body's `bridge { … }` block already lists the
/// glob `<root>`, with either grammar-admitted optional separator tail.
pub fn package_lists_bridge_glob(package_file: &str, root: &str) -> bool {
    package_file
        .lines()
        .map(str::trim)
        .any(|line| line == root || line.strip_suffix(';') == Some(root))
}

const ELABORATOR_IMPORTS: &str = "\
import algebraic_elaborators(iso, into, onto, align, ease, atom);
import spine_elaborators(reorder_sum, reorder_prod, narrow_sum, narrow_prod, widen_sum, widen_prod, flatten_sum, flatten_prod, one_sum, one_prod, fit);
import match(match);
import control(if, scope);
";

const ELABORATOR_SUPPORT_FILES: &[(&str, &str)] = &[
    (
        "workdir/elaborator_util.kio",
        include_str!("../../../../test-data/poc/elab/workdir/elaborator_util.kio"),
    ),
    (
        "workdir/spine_elaborators.kio",
        include_str!("../../../../test-data/poc/elab/workdir/spine_elaborators.kio"),
    ),
    (
        "workdir/algebraic_elaborators.kio",
        include_str!("../../../../test-data/poc/elab/workdir/algebraic_elaborators.kio"),
    ),
    (
        "workdir/match.kio",
        include_str!("../../../../test-data/poc/elab/workdir/match.kio"),
    ),
    (
        "workdir/control.kio",
        include_str!("../../../../test-data/poc/elab/workdir/control.kio"),
    ),
    // `algebraic_elaborators` / `spine_elaborators` import their host
    // role types (`String` / `I32` / `Int`) from `testapi`, so the
    // bundle must carry a `testapi` module declaring them. The
    // generated program references only those passthrough role types
    // (never `testapi`'s opaque `host type Token;`), so the bundled
    // module declares exactly the role types the elaborators import —
    // all passthrough, leaving the package boundary free of opaque
    // associated types the default (`empty`-tier) runner host doesn't
    // supply. `testapi` declares `host` items and is reachable from the
    // bridged `prog` module, so it is also bridged (see the
    // surface-mode `bridge { … }` block in `render_package_files`).
    ("workdir/testapi.kio", GENERATED_TESTAPI_KIO),
];

// The bundled `testapi` host-type module. Declares only the
// passthrough role types the bundled elaborator modules import
// (`String` / `I32` / `Int`); deliberately omits the role-less opaque
// `host type Token;` of the POC `testapi` so the generated program's
// host boundary stays free of associated types the `empty`-tier runner
// host cannot define.
const GENERATED_TESTAPI_KIO: &str = "\
module testapi;

host type String role(str);

host type I32 role(i32);

host type Int role(i32);

host type Bool role(bool);
";

const GENERATED_ELABORATORS_PATH: &str = "workdir/kio_gen_elaborators.kio";

pub fn add_elaborator_support_files(files: &mut Vec<(String, String)>) {
    for (path, content) in ELABORATOR_SUPPORT_FILES {
        if files.iter().any(|(existing, _)| existing == path) {
            continue;
        }
        files.push(((*path).to_owned(), (*content).to_owned()));
    }
}

// kio-rs doesn't yet support dotted-path value access through an
// imported newtype, so each constructor / projector is wrapped in a
// flat function defined alongside the newtype. Generated bodies call
// those wrappers as ordinary functions.
const UTIL_KIO_BASE: &str = "\
pub pure fn id[A](x: A) -> A { x }
pub pure fn const1[A][B](x: A, y: B) -> A { x }
pub pure fn const2[A][B](x: A, y: B) -> B { y }

pub newtype I32_box  : I32    { pub constructor mk_i32_box;  pub projector un_i32_box  }
pub newtype _I32_box : I32    { pub constructor mk_marked_i32_box; pub projector un_marked_i32_box }
pub newtype Strbox  : String { pub constructor mk_strbox;  pub projector un_strbox  }
pub newtype Boolbox : Bool   { pub constructor mk_boolbox; pub projector un_boolbox }

pub pure fn box_i32(x: I32) -> I32_box { I32_box.mk_i32_box(x) }
pub pure fn unbox_i32(b: I32_box) -> I32 { I32_box.un_i32_box(b) }
pub pure fn box_marked_i32(x: I32) -> _I32_box { _I32_box.mk_marked_i32_box(x) }
pub pure fn unbox_marked_i32(b: _I32_box) -> I32 { _I32_box.un_marked_i32_box(b) }
pub pure fn box_str(x: String) -> Strbox { Strbox.mk_strbox(x) }
pub pure fn unbox_str(b: Strbox) -> String { Strbox.un_strbox(b) }
pub pure fn box_bool(x: Bool) -> Boolbox { Boolbox.mk_boolbox(x) }
pub pure fn unbox_bool(b: Boolbox) -> Bool { Boolbox.un_boolbox(b) }

pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }
pub pure fn box_any[A](x: A) -> Box(A) { Box.mk_box(A, x) }
pub pure fn unbox_any[A](x: Box(A)) -> A { Box.un_box(A, x) }

// Recursive newtypes that exercise strict-positivity and the
// iso-recursive wrap/unwrap boundary in the typechecker. The
// generator declares these but never builds values of them —
// building values would need a way around the call-site complex-
// type-arg limit (intrinsics like `__left__` would need a paren-
// wrapped second type arg, and the parser routes paren-leading args
// to value position) — bounded value generation through a different
// path is a follow-up.
pub rec newtype Strlist : (. | (String & Strlist))
  { pub constructor mk_strlist; pub projector un_strlist }
pub rec newtype Inttree : (. | (I32 & (Inttree & Inttree)))
  { pub constructor mk_inttree; pub projector un_inttree }

rec {
  pub type Kio_gen_cycle = Kio_gen_node;
  pub newtype Kio_gen_node : Kio_gen_cycle
    { pub constructor mk_kio_gen_node; pub projector un_kio_gen_node }
}

pub type Intpair = (I32 & I32);
pub type Optint  = (. | I32);

// A monomorphic 2-arg fn used as the binding target for the
// generator's surface-only `op` declaration (see UTIL_KIO_OP
// below). Returns its first arg, so the result type is always I32
// regardless of the second arg's relevance — keeping the op-chain
// use sites trivially well-typed.
pub pure fn kio_gen_op_pick(x: I32, y: I32) -> I32 { x }
";

// Labels block appended to the generated root module for surface-using cases.
// `labels` is a surface form (not Kio'), so this fragment must NOT
// appear in cases marked `IS_KIO_PRIME`. label_elab synthesises one
// generated newtype per explicit declaration, each exposing `mk` and `get`
// members; `_` entries reuse those declarations. The forwarding binding names
// the existing count label without minting a nominal or uppercase name.
const UTIL_KIO_TAGS: &str = "
pub labels { greet : String, count : I32 };
pub labels Kio_gen_labels = { greet : _ } | { count : _ };
pub type {kio_gen_count_forward} = {count};
";

// Literal aliases appended to the generated root module for surface-using cases.
// Each `literal name = <literal>;` stores an untyped literal token;
// use sites render as `name(Type)` when the generated expression
// needs the same explicit annotation as the raw literal. Surface
// form only: `IS_KIO_PRIME` cases must not see these.
const UTIL_KIO_LITERAL_ALIASES: &str = "
pub literal zero_i32 = 0;
pub literal hello_str = \"hello\";
";

// The local declaration and its generated chains share the complete binary
// grammar `_ <+> _`; a consumer selects it as `import prog(op _ <+> _);`.
const UTIL_KIO_OP: &str = "
pub op _ <+> _ { impl kio_gen_op_pick }
";

// A surface-only seeded variadic operator. The seed consumes the rightmost
// element before ordinary right-fold steps. The step deliberately
// returns zero so separate singleton and two-element equivalence claims prove
// both that the terminal element is not stepped and that remaining elements
// are stepped.
const UTIL_KIO_VARIADIC: &str = "
pub pure fn kio_gen_fold_seed(value: I32) -> I32 { value }
pub pure fn kio_gen_fold_step(_value: I32, _acc: I32) -> I32 { 0(I32) }
pub varop [* *] { foldr1 kio_gen_fold_step kio_gen_fold_seed }
";

fn util_kio(prog: &Program) -> String {
    // The labels block is gated on `surface_mode` rather than
    // `uses_surface`: the generator might pick `Type::Label` types in
    // the signature (and even invoke generated label members in the body)
    // without rendering any visible surface form, in which case
    // `uses_surface` ends up false but the program still references
    // the generated label types. Including / excluding the labels has to
    // track the wrapper-usage decision, which is what `surface_mode`
    // records.
    if prog.surface_mode {
        format!(
            "{UTIL_KIO_BASE}{UTIL_KIO_TAGS}{UTIL_KIO_LITERAL_ALIASES}{UTIL_KIO_OP}{UTIL_KIO_VARIADIC}"
        )
    } else {
        UTIL_KIO_BASE.to_string()
    }
}

fn generated_elaborator_imports(prog: &Program) -> String {
    if !prog.surface_mode || prog.user_elaborators.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = prog
        .user_elaborators
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    format!("import kio_gen_elaborators({});\n", names.join(", "))
}

fn render_generated_elaborator_module(prog: &Program) -> Option<String> {
    if !prog.surface_mode || prog.user_elaborators.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\
module kio_gen_elaborators;

import __intrinsics__;

import __comptime__;

pub type Kio_gen_capture = .;

pub fn kio_gen_capture_value() -> . { () }

type Target_request = __Type__ | .;
",
    );
    for spec in &prog.user_elaborators {
        out.push('\n');
        out.push_str(&render_generated_elaborator_impl(spec));
        out.push('\n');
        out.push('\n');
        out.push_str(&render_generated_elaborator_decl(spec));
        out.push('\n');
    }
    Some(out)
}

fn render_generated_elaborator_decl(spec: &UserElaboratorSpec) -> String {
    let captures = if spec.captures.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = spec
            .captures
            .iter()
            .map(|capture| match capture {
                UserElaboratorCapture::TypeAlias => "Kio_gen_capture",
                UserElaboratorCapture::ValueFn => "kio_gen_capture_value",
            })
            .collect();
        format!(" captures ({});", names.join(", "))
    };
    let marker = match spec.implementation {
        UserElaboratorImplementation::Late(_) => "",
        UserElaboratorImplementation::FillsIdentity => "(fills)",
    };
    format!(
        "pub elab {} : [{}] {} -> [{}] {} {{{captures} impl{marker} {} }}",
        spec.name,
        spec.source_param,
        spec.source_param,
        spec.target_param,
        spec.target_param,
        spec.impl_name
    )
}

fn render_generated_elaborator_impl(spec: &UserElaboratorSpec) -> String {
    if matches!(
        spec.implementation,
        UserElaboratorImplementation::FillsIdentity
    ) {
        assert!(
            spec.captures.is_empty(),
            "the fixed fills elaborator must remain capture-free"
        );
        return format!(
            "pure fn {}(ct: __Comptime__, fills: __Fill_ctx__, source: __Type__, value: __Checked_term__, target: __Type__) -> __Checked_term__ & __Fill_ctx__ {{\n  (value, __fill__(ct, fills, target, source))\n}}",
            spec.impl_name
        );
    }

    let UserElaboratorImplementation::Late(template) = spec.implementation else {
        unreachable!("the fills implementation returned above")
    };
    let mut params = vec!["ct: __Comptime__"];
    for capture in &spec.captures {
        match capture {
            UserElaboratorCapture::TypeAlias => params.push("captured_type: __Type__"),
            UserElaboratorCapture::ValueFn => params.push("captured_value: __Checked_term__"),
        }
    }
    params.extend([
        "source: __Type__",
        "value: __Checked_term__",
        "target: Target_request",
    ]);

    let mut lines = Vec::new();
    if spec.captures.contains(&UserElaboratorCapture::TypeAlias) {
        lines.push(
            "let _captured_type_seen = __type_equal__(ct, captured_type, __reflect_type__(ct, Kio_gen_capture));"
                .to_string(),
        );
    }
    if spec.captures.contains(&UserElaboratorCapture::ValueFn) {
        lines.push("let _captured_value_type = __term_type__(ct, captured_value);".to_string());
    }
    lines.push(render_generated_elaborator_body(template));

    format!(
        "pure fn {}({}) -> __Checked_term__ {{\n{}\n}}",
        spec.impl_name,
        params.join(", "),
        indent_lines(&lines.join("\n"), 2)
    )
}

fn generated_fills_witness(prog: &Program) -> String {
    let Some(spec) = prog.user_elaborators.iter().find(|spec| {
        matches!(
            spec.implementation,
            UserElaboratorImplementation::FillsIdentity
        )
    }) else {
        return String::new();
    };
    format!(
        "\nfn kio_gen_fills_witness(value: I32) -> I32 {{ {name}!(value, _) }}\n\
         fn kio_gen_polymorphic_fills_witness(value: I32) -> [A] A -> I32 {{ {name}!(.[B](_ignored: B) {{ value }}, _) }}\n\
         fn kio_gen_callback_apply[A][B](callback: A -> B, value: A) -> B {{ callback(value) }}\n\
         fn kio_gen_callback_fills_witness(value: I32) -> I32 {{ {name}!(kio_gen_callback_apply(.(input: I32) -> I32 {{ input }}, value), _) }}\n",
        name = spec.name
    )
}

fn render_generated_elaborator_body(template: UserElaboratorTemplate) -> String {
    match template {
        UserElaboratorTemplate::Direct => "value".to_string(),
        UserElaboratorTemplate::TargetBranch => "\
__either__(
  , __Type__
  , .
  , __Checked_term__
  , target
  , .(_target: __Type__) -> __Checked_term__ { value }
  , .() -> __Checked_term__ { value }
  )"
        .to_string(),
        UserElaboratorTemplate::SourceTargetGuard => "\
__either__(
  , __Type__
  , .
  , __Checked_term__
    , target
    , .(target_type: __Type__) -> __Checked_term__ {
    __if_then_else__(
      , __type_equal__(ct, source, target_type)
      , .() -> __Checked_term__ { value }
      , .() -> __Checked_term__ { __type_error__(ct, \"kio-gen source/target mismatch\") }
      )
  }
  , .() -> __Checked_term__ { value }
  )"
        .to_string(),
        UserElaboratorTemplate::TermTypeGuard => "\
__either__(
  , __Type__
  , .
  , __Checked_term__
    , target
    , .(target_type: __Type__) -> __Checked_term__ {
    __if_then_else__(
      , __type_equal__(ct, __term_type__(ct, value), target_type)
      , .() -> __Checked_term__ { value }
      , .() -> __Checked_term__ { __type_error__(ct, \"kio-gen term/target mismatch\") }
      )
  }
  , .() -> __Checked_term__ { value }
  )"
        .to_string(),
        UserElaboratorTemplate::LetIdentity => "\
__term_let__(
  , ct
  , source
  , value
  , .(bound: __Checked_term__) -> __Checked_term__ { bound }
  )"
        .to_string(),
        UserElaboratorTemplate::PairRoundTrip => "\
let pair_type = __type_product__(ct, source, __type_unit__(ct));
__intrinsic_fst__(
  , ct
  , pair_type
  , __intrinsic_pair__(ct, value, __term_unit__(ct))
  )"
        .to_string(),
        UserElaboratorTemplate::SumRoundTrip => "\
let sum_type = __type_sum__(ct, source, __type_bottom__(ct));
__intrinsic_either__(
  , ct
  , sum_type
  , source
  , __intrinsic_left__(ct, sum_type, value)
  , .(payload: __Checked_term__) -> __Checked_term__ { payload }
  , .(bottom: __Checked_term__) -> __Checked_term__ { __intrinsic_absurd__(ct, bottom, source) }
  )"
        .to_string(),
    }
}

fn indent_lines(s: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    s.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a program as the (relative-path, content) pairs that make
/// up its package. Used directly for valid cases; mutators take the
/// same vector and edit one of the entries to produce an invalid
/// case.
pub fn render_package_files(prog: &Program) -> Vec<(String, String)> {
    // Append fixed `equiv` claims in surface mode. Per
    // `specs/language.md` § Equivalence claims, `equiv` items are
    // filtered out at the Lowered→Prime boundary — they contribute
    // nothing to the build artifact but exercise the equiv parser
    // and typechecker. The standard run.args path runs `kio test`
    // before build/run for generated surface cases, so the claims check
    // Unit identity, seeded folds and the forwarded label's original identity.
    let equiv_block = if prog.surface_mode {
        "\nequiv kio_gen_unit_equiv { (); id(., ()) }\n\
equiv kio_gen_seeded_fold_singleton { [* 2(I32) *]; 2(I32) }\n\
equiv kio_gen_seeded_fold_step { [* 1(I32), 2(I32) *]; 0(I32) }\n\
equiv kio_gen_label_forward_nominal { {kio_gen_count_forward = 7(I32)}; Count.mk(7(I32)) }\n\
equiv kio_gen_label_forward_payload { Count.mk(11(I32)).?{kio_gen_count_forward}; 11(I32) }\n"
    } else {
        ""
    };
    // Surface-mode programs may emit structural/algebraic elaborator
    // forms plus `match!`; each call site imports its elaborator from
    // an explicit same-package implementation. Kio'-mode
    // programs carry no elaborator forms, so these imports and support
    // files are surface-only.
    let elaborator_imports = if prog.surface_mode {
        format!("{ELABORATOR_IMPORTS}{}", generated_elaborator_imports(prog))
    } else {
        String::new()
    };
    // The package's `bridge { … }` block selects the modules whose
    // `host` items form the package boundary. The generated root module
    // `prog` always declares host items (the base role types). In
    // surface mode the bundled `testapi` support module also declares
    // host items and is reachable from `prog` through the elaborator
    // imports, so the bridge-completeness check requires bridging it
    // too; the other elaborator modules carry no host items and need no
    // glob.
    let package_file = if prog.surface_mode {
        format!(
            "{PACKAGE_FILE_KIO_PREFIX}\n{}",
            bridge_glob_block(["prog", "testapi"])
        )
    } else {
        format!("{PACKAGE_FILE_KIO_PREFIX}\n{}", bridge_glob_block(["prog"]))
    };
    let fills_witness = generated_fills_witness(prog);
    // Import clauses lead the module body (the module grammar requires
    // them before any item), so the `host type` declarations follow the
    // imports rather than preceding them.
    let root_module = format!(
        "\
module prog;

import __intrinsics__;
{}
{HOST_ENV_KIO}
{}
{fills_witness}
pub fn main() -> . {{ () }}

{}{equiv_block}",
        elaborator_imports,
        util_kio(prog),
        render::render_fn_def(prog)
    );
    // Each module's file sits at the path its `module <path>;`
    // declaration implies relative to the package root (`workdir/`): under
    // the module-name rules in `specs/package.md`, the declared
    // segments equal the file's path relative to the package root with
    // directory separators written as `/`.
    let mut files = vec![
        ("workdir/prog.pkg.kio".to_string(), package_file),
        ("workdir/prog.kio".to_string(), root_module),
    ];
    if prog.surface_mode {
        add_elaborator_support_files(&mut files);
        if let Some(content) = render_generated_elaborator_module(prog) {
            files.push((GENERATED_ELABORATORS_PATH.to_string(), content));
        }
    }
    files
}

/// Write a case directory under `<out_root>/<category>/<case_name>/`,
/// laying down the package files plus the expected.* / IS_KIO_PRIME
/// markers (including `expected.stderr.ignore`) and run.args.
///
/// `is_kio_prime` decides whether the `IS_KIO_PRIME` marker is
/// dropped.
pub fn write_case(
    out_root: &Path,
    category: &str,
    case_name: &str,
    files: &[(String, String)],
    exit_code: u32,
    is_kio_prime: bool,
    runner_protocol: &str,
) -> io::Result<()> {
    let case_dir = out_root.join(category).join(case_name);
    fs::create_dir_all(&case_dir)?;

    if is_kio_prime {
        fs::write(case_dir.join("IS_KIO_PRIME"), "")?;
    }
    fs::write(case_dir.join("expected.stderr.ignore"), "")?;
    fs::write(case_dir.join("expected.exit"), format!("{exit_code}\n"))?;
    fs::write(case_dir.join("expected.stdout"), "")?;

    fs::write(
        case_dir.join("run.args"),
        format!("--protocol\n{runner_protocol}\n"),
    )?;

    for (rel, content) in files {
        let path = case_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
    }

    Ok(())
}

/// Convenience for the (overwhelmingly common) success case.
pub fn write_valid_case(out_root: &Path, case_name: &str, prog: &Program) -> io::Result<()> {
    let files = render_package_files(prog);
    write_case(
        out_root,
        "00_success",
        case_name,
        &files,
        0,
        !prog.surface_mode,
        runner_protocol(prog.surface_mode),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kio_gen::ast::{Term, Type};

    fn elaborator(captures: Vec<UserElaboratorCapture>) -> UserElaboratorSpec {
        UserElaboratorSpec {
            name: "generated".to_string(),
            impl_name: "generated_impl".to_string(),
            source_param: "Source".to_string(),
            target_param: "Target".to_string(),
            implementation: UserElaboratorImplementation::Late(UserElaboratorTemplate::Direct),
            captures,
        }
    }

    fn program_with(spec: UserElaboratorSpec) -> Program {
        Program {
            fn_def_name: "gen".to_string(),
            params: Vec::new(),
            ret: Type::Unit,
            body: Term::UnitLit,
            user_elaborators: vec![spec],
            uses_surface: true,
            surface_mode: true,
        }
    }

    fn module_with(spec: UserElaboratorSpec) -> String {
        render_generated_elaborator_module(&program_with(spec)).expect("surface elaborator module")
    }

    #[test]
    fn package_and_bridge_emit_between_item_separators_only() {
        assert_eq!(
            PACKAGE_FILE_KIO_PREFIX,
            r#"package prog;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
  };

  target ts {
    out "out/ts/"
  };

  target python {
    out "out/python/"
  };

  target java {
    out "out/java/"
  };

  target rust {
    out "out/rust/"
  }
}
"#
        );
        assert_eq!(
            bridge_glob_block(["prog", "testapi"]),
            "bridge {\n  prog;\n  testapi\n}\n"
        );
        assert_eq!(bridge_glob_block(["prog"]), "bridge {\n  prog\n}\n");
    }

    #[test]
    fn bridge_glob_lookup_accepts_both_grammar_admitted_tails() {
        assert!(package_lists_bridge_glob("bridge {\n  prog\n}\n", "prog"));
        assert!(package_lists_bridge_glob("bridge {\n  prog;\n}\n", "prog"));
        assert!(!package_lists_bridge_glob(
            "bridge {\n  prog;;\n}\n",
            "prog"
        ));
        assert!(!package_lists_bridge_glob(
            "bridge {\n  testapi\n}\n",
            "prog"
        ));
    }

    #[test]
    fn generated_braced_declarations_omit_final_semicolons() {
        assert!(UTIL_KIO_BASE.contains(
            "pub newtype I32_box  : I32    { pub constructor mk_i32_box;  pub projector un_i32_box  }\n"
        ));
        assert!(UTIL_KIO_BASE.contains(
            "pub rec newtype Strlist : (. | (String & Strlist))\n  { pub constructor mk_strlist; pub projector un_strlist }\n"
        ));
        assert!(UTIL_KIO_BASE.contains(
            "rec {\n  pub type Kio_gen_cycle = Kio_gen_node;\n  pub newtype Kio_gen_node : Kio_gen_cycle\n    { pub constructor mk_kio_gen_node; pub projector un_kio_gen_node }\n}\n"
        ));
        assert_eq!(UTIL_KIO_OP, "\npub op _ <+> _ { impl kio_gen_op_pick }\n");
        assert_eq!(
            UTIL_KIO_VARIADIC,
            "\npub pure fn kio_gen_fold_seed(value: I32) -> I32 { value }\npub pure fn kio_gen_fold_step(_value: I32, _acc: I32) -> I32 { 0(I32) }\npub varop [* *] { foldr1 kio_gen_fold_step kio_gen_fold_seed }\n"
        );
    }

    #[test]
    fn generated_imports_are_module_first_and_precede_items() {
        let prog = program_with(elaborator(Vec::new()));
        let files = render_package_files(&prog);
        let root = files
            .iter()
            .find(|(path, _)| path == "workdir/prog.kio")
            .map(|(_, source)| source.as_str())
            .expect("generated root module");
        let first_item = root.find("host type I8").expect("first host item");
        for import in [
            "import __intrinsics__;",
            "import algebraic_elaborators(iso, into, onto, align, ease, atom);",
            "import spine_elaborators(reorder_sum, reorder_prod, narrow_sum, narrow_prod, widen_sum, widen_prod, flatten_sum, flatten_prod, one_sum, one_prod, fit);",
            "import match(match);",
            "import control(if, scope);",
            "import kio_gen_elaborators(generated);",
        ] {
            assert_eq!(root.matches(import).count(), 1, "{import}");
            assert!(root.find(import).unwrap() < first_item, "{import}");
        }
        let generated = module_with(elaborator(Vec::new()));
        let first_item = generated.find("pub type Kio_gen_capture").unwrap();
        for import in ["import __intrinsics__;", "import __comptime__;"] {
            assert_eq!(generated.matches(import).count(), 1);
            assert!(generated.find(import).unwrap() < first_item);
        }
    }

    #[test]
    fn public_elaborator_has_private_capture_free_implementation() {
        let generated = module_with(elaborator(Vec::new()));

        assert!(generated.contains("pure fn generated_impl(ct: __Comptime__"));
        assert!(!generated.contains("pub pure fn generated_impl(ct: __Comptime__"));
        assert!(generated.contains("pub elab generated : [Source] Source -> [Target] Target"));
        assert!(generated.contains(
            "pub elab generated : [Source] Source -> [Target] Target { impl generated_impl }\n"
        ));
        assert!(!generated.contains("captures ("));
    }

    #[test]
    fn public_elaborator_has_public_captures_and_private_implementation() {
        let generated = module_with(elaborator(vec![
            UserElaboratorCapture::TypeAlias,
            UserElaboratorCapture::ValueFn,
        ]));

        assert!(generated.contains("pub type Kio_gen_capture = .;"));
        assert!(generated.contains("pub fn kio_gen_capture_value() -> ."));
        assert!(generated.contains("pure fn generated_impl(ct: __Comptime__"));
        assert!(!generated.contains("pub pure fn generated_impl(ct: __Comptime__"));
        assert!(generated.contains("captures (Kio_gen_capture, kio_gen_capture_value);"));
    }

    #[test]
    fn fills_elaborator_renders_its_exact_marker_abi_body_and_witness() {
        let spec = UserElaboratorSpec {
            name: "generated_fills".to_owned(),
            impl_name: "generated_fills_impl".to_owned(),
            source_param: "Source".to_owned(),
            target_param: "Target".to_owned(),
            implementation: UserElaboratorImplementation::FillsIdentity,
            captures: Vec::new(),
        };
        let prog = program_with(spec);
        let generated =
            render_generated_elaborator_module(&prog).expect("surface elaborator module");
        assert!(generated.contains(
            "pure fn generated_fills_impl(ct: __Comptime__, fills: __Fill_ctx__, source: __Type__, value: __Checked_term__, target: __Type__) -> __Checked_term__ & __Fill_ctx__ {\n  (value, __fill__(ct, fills, target, source))\n}"
        ));
        assert!(generated.contains(
            "pub elab generated_fills : [Source] Source -> [Target] Target { impl(fills) generated_fills_impl }"
        ));

        let files = render_package_files(&prog);
        let root = files
            .iter()
            .find(|(path, _)| path == "workdir/prog.kio")
            .map(|(_, source)| source.as_str())
            .expect("generated root module");
        assert!(root.contains(
            "fn kio_gen_fills_witness(value: I32) -> I32 { generated_fills!(value, _) }"
        ));
        assert!(root.contains(
            "fn kio_gen_polymorphic_fills_witness(value: I32) -> [A] A -> I32 { generated_fills!(.[B](_ignored: B) { value }, _) }"
        ));
        assert!(root.contains(
            "fn kio_gen_callback_apply[A][B](callback: A -> B, value: A) -> B { callback(value) }"
        ));
        assert!(root.contains(
            "fn kio_gen_callback_fills_witness(value: I32) -> I32 { generated_fills!(kio_gen_callback_apply(.(input: I32) -> I32 { input }, value), _) }"
        ));
    }

    #[test]
    fn generated_modes_select_exact_runner_protocols() {
        assert_eq!(runner_protocol(false), "generated-core-main");
        assert_eq!(runner_protocol(true), "generated-surface-main");
    }

    #[test]
    fn generated_elaborator_templates_use_unit_as_a_type_only_via_dot() {
        for template in [
            UserElaboratorTemplate::TargetBranch,
            UserElaboratorTemplate::SourceTargetGuard,
            UserElaboratorTemplate::TermTypeGuard,
        ] {
            let generated = render_generated_elaborator_body(template);
            assert!(
                generated.contains("__either__(\n  , __Type__\n  , .\n"),
                "generated template must spell the Unit type as `.`: {generated}"
            );
            assert!(
                !generated.contains("__either__(\n  , __Type__\n  , ()\n"),
                "generated template used the Unit value in a type slot: {generated}"
            );
        }
    }
}
