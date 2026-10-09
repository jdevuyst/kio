#[path = "../manual_pack.rs"]
mod manual_pack;

fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let check = match args.as_slice() {
        [flag] if flag == "--check" => true,
        [flag] if flag == "--write" => false,
        _ => return Err(std::io::Error::other("usage: pack_manual --check|--write")),
    };
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !manual_pack::is_checkout(crate_root) {
        return Err(std::io::Error::other(
            "manual packing requires a source checkout",
        ));
    }
    let bundle = manual_pack::assemble(crate_root.parent().unwrap())?;
    manual_pack::validate(&bundle)?;
    manual_pack::snapshot(&bundle, &crate_root.join("manual"), check)
}
