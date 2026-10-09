use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};

const INDEX: &str = include_str!(concat!(env!("OUT_DIR"), "/index.tsv"));
const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.tar.gz-concat"));

const HELP: &str =
    "Usage: kio manual [ls [path] | tree [path] | cat <path> | export [--format=FORMAT]]

Read bundled documentation offline.

Subcommands:
  ls       List immediate children (default: virtual root).
  tree     List the complete subtree (default: virtual root).
  cat      Write one complete file's original bytes to stdout.
  export   Write the complete archive to stdout; refuses a terminal.

Options:
  -h, --help    Show this help and exit.

Start reading:
  kio manual cat docs/README.md
  kio manual cat KIO-FOR-AGENTS.md

Discover contents: kio manual tree
Archive formats and extraction: kio manual export --help";

fn help(operation: Option<&str>) -> String {
    let body = match operation {
        Some("export") => EXPORT_HELP.to_owned(),
        Some("cat") => "Usage: kio manual cat <path>\n\nWrite one complete bundled file's exact bytes to stdout.\nDiscover paths: kio manual tree".into(),
        Some("ls") => "Usage: kio manual ls [path]\n\nList immediate children of a bundled directory, one per line.\nThe default is the virtual root; directory names end with /.".into(),
        Some("tree") => "Usage: kio manual tree [path]\n\nList the complete bundled subtree using ASCII indentation.\nThe default is the virtual root; directory names end with /.".into(),
        _ => HELP.to_owned(),
    };
    kio_lang::documentation_help(&format!(
        "{body}\n\nExit codes (per {{base}}/specs/exit-codes.md): 0 on success; 2 on invalid\narguments, missing bundled paths, or binary output to a terminal;\n1 on archive or output I/O failure.\n\nSee {{base}}/specs/cli.md#kio-manual for full command behavior."
    ))
}

const EXPORT_HELP: &str = "Usage: kio manual export [--format=FORMAT]

Exports KIO-FOR-AGENTS.md and the bundled docs/ and specs/ trees.
Archive contents match: kio manual tree
Default format: tar.gz-concat (concatenated single-file tar.gz archives).
Prefer tar.gz-concat for direct compressed export, or tar for immediate
local extraction without recompression. Choose tar.gz when a conventional
compressed archive is needed.

Extract the default format into the current directory:
  kio manual export | tar --ignore-zeros -xzf -

The --ignore-zeros option reads past each inner tar archive's end marker.
Without it, extraction may succeed but produce only the first file.
The final - tells tar to read the archive from standard input.

Extract a conventional tar archive:
  kio manual export --format=tar | tar -xf -

Extract a conventional compressed archive:
  kio manual export --format=tar.gz | tar -xzf -";

struct Entry<'a> {
    path: &'a str,
    size: u64,
    offset: usize,
    length: usize,
}

fn entries() -> Vec<Entry<'static>> {
    let mut lines = INDEX.lines();
    assert_eq!(lines.next(), Some("kio-manual-v1"), "manual index version");
    lines
        .filter(|line| !line.starts_with("identity\t"))
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 4, "manual index fields");
            Entry {
                path: fields[0],
                size: fields[1].parse().expect("manual file size"),
                offset: fields[2].parse().expect("manual file offset"),
                length: fields[3].parse().expect("manual archive size"),
            }
        })
        .collect()
}

fn path(input: &str) -> Result<&str, String> {
    if input.starts_with('/') {
        return Err(format!(
            "absolute bundled paths are not accepted: {input:?}"
        ));
    }
    let input = input.strip_suffix('/').unwrap_or(input);
    if input.is_empty() || input == "." {
        return Ok("");
    }
    if input.contains('\\')
        || input.chars().any(char::is_control)
        || input
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!(
            "invalid bundled path {input:?}; use kio manual tree"
        ));
    }
    Ok(input)
}

fn subtree<'a>(entries: &'a [Entry<'a>], path: &str) -> Result<Vec<&'a Entry<'a>>, String> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let selected: Vec<_> = entries
        .iter()
        .filter(|entry| entry.path.starts_with(&prefix))
        .collect();
    if selected.is_empty() {
        let kind = if entries.iter().any(|entry| entry.path == path) {
            "not a directory"
        } else {
            "not found"
        };
        return Err(format!(
            "bundled directory `{path}` {kind}; use kio manual tree"
        ));
    }
    Ok(selected)
}

fn list(
    entries: &[Entry<'_>],
    path: &str,
    recursive: bool,
    output: &mut impl Write,
) -> io::Result<()> {
    let selected = subtree(entries, path).map_err(io::Error::other)?;
    let prefix_len = if path.is_empty() { 0 } else { path.len() + 1 };
    let mut names = BTreeSet::new();
    for entry in selected {
        let relative = &entry.path[prefix_len..];
        let parts: Vec<_> = relative.split('/').collect();
        if recursive {
            for count in 1..=parts.len() {
                names.insert(format!(
                    "{}{}",
                    parts[..count].join("/"),
                    if count != parts.len() { "/" } else { "" }
                ));
            }
        } else {
            names.insert(format!(
                "{}{}",
                parts[0],
                if parts.len() > 1 { "/" } else { "" }
            ));
        }
    }
    for name in names {
        let path = name.trim_end_matches('/');
        let depth = if recursive {
            path.matches('/').count()
        } else {
            0
        };
        writeln!(
            output,
            "{}{}{}",
            "  ".repeat(depth),
            path.rsplit('/').next().unwrap(),
            if name.ends_with('/') { "/" } else { "" }
        )?;
    }
    Ok(())
}

fn archive<'a>(
    entry: &Entry<'_>,
    payload: &'a [u8],
) -> tar::Archive<flate2::read::GzDecoder<&'a [u8]>> {
    let bytes = payload
        .get(
            entry.offset
                ..entry
                    .offset
                    .checked_add(entry.length)
                    .expect("manual archive bounds"),
        )
        .expect("manual archive within payload");
    tar::Archive::new(flate2::read::GzDecoder::new(bytes))
}

fn cat(entry: &Entry<'_>, payload: &[u8], output: &mut impl Write) -> io::Result<()> {
    let mut archive = archive(entry, payload);
    let mut entries = archive.entries()?;
    let mut file = entries
        .next()
        .ok_or_else(|| io::Error::other("empty bundled archive"))??;
    if file.path()?.as_ref() != std::path::Path::new(entry.path) || file.size() != entry.size {
        return Err(io::Error::other("bundled archive does not match its index"));
    }
    if io::copy(&mut file, output)? != entry.size {
        return Err(io::Error::other("truncated bundled file"));
    }
    drop(file);
    if entries.next().is_some() {
        return Err(io::Error::other("multiple entries in bundled file archive"));
    }
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    Ok(())
}

fn tar(entries: &[Entry<'_>], payload: &[u8], output: &mut impl Write) -> io::Result<()> {
    let mut destination = tar::Builder::new(output);
    for entry in entries {
        let mut source = archive(entry, payload);
        let mut files = source.entries()?;
        let mut file = files
            .next()
            .ok_or_else(|| io::Error::other("empty bundled archive"))??;
        if file.path()?.as_ref() != std::path::Path::new(entry.path) || file.size() != entry.size {
            return Err(io::Error::other("bundled archive does not match its index"));
        }
        let mut header = file.header().clone();
        destination.append_data(&mut header, entry.path, &mut file)?;
        drop(file);
        if files.next().is_some() {
            return Err(io::Error::other("multiple entries in bundled file archive"));
        }
        io::copy(&mut source.into_inner(), &mut io::sink())?;
    }
    destination.finish()
}

pub fn run(args: &[String]) -> i32 {
    let mut output = io::stdout().lock();
    match execute(args, &mut output, io::stdout().is_terminal()) {
        Ok(()) => match output.flush() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: writing bundled manual: {error}");
                1
            }
        },
        Err((code, message)) => {
            eprintln!("error: {message}");
            code
        }
    }
}

fn execute(args: &[String], output: &mut impl Write, terminal: bool) -> Result<(), (i32, String)> {
    let usage = |message: String| (2, message);
    let io_error = |error: io::Error| (1, format!("reading or writing bundled manual: {error}"));
    let Some(operation) = args.first().map(String::as_str) else {
        writeln!(output, "Bundled documentation for {} (bundle {})\n\nOperations: ls, tree, cat, export\nRead: kio manual cat docs/README.md\nAgent workflow: kio manual cat KIO-FOR-AGENTS.md\nDiscover: kio manual tree\nHelp: kio manual --help",
            kio_lang::version_line(false), INDEX.lines().last().expect("manual identity").strip_prefix("identity\t").expect("manual identity field")).map_err(io_error)?;
        return Ok(());
    };
    if matches!(operation, "-h" | "--help") && args.len() == 1 {
        writeln!(output, "{}", help(None)).map_err(io_error)?;
        return Ok(());
    }
    if args.len() == 2 && matches!(args[1].as_str(), "-h" | "--help") {
        if !matches!(operation, "export" | "ls" | "tree" | "cat") {
            return Err(usage(format!("unknown manual operation {operation:?}")));
        }
        writeln!(output, "{}", help(Some(operation))).map_err(io_error)?;
        return Ok(());
    }
    let entries = entries();
    match operation {
        "ls" | "tree" if args.len() <= 2 => {
            let selected = path(args.get(1).map(String::as_str).unwrap_or("")).map_err(usage)?;
            subtree(&entries, selected).map_err(usage)?;
            list(&entries, selected, operation == "tree", output).map_err(io_error)
        }
        "cat" if args.len() == 2 => {
            if args[1].ends_with('/') {
                return Err(usage(
                    "a bundled file path must not end with /; use kio manual tree".into(),
                ));
            }
            let selected = path(&args[1]).map_err(usage)?;
            let entry = entries
                .iter()
                .find(|entry| entry.path == selected)
                .ok_or_else(|| {
                    usage(format!(
                        "bundled file `{selected}` not found; use kio manual tree"
                    ))
                })?;
            cat(entry, PAYLOAD, output).map_err(io_error)
        }
        "export" => {
            let format = match &args[1..] {
                [] => "tar.gz-concat",
                [flag] if flag.starts_with("--format=") => &flag[9..],
                [flag, value] if flag == "--format" => value,
                _ => {
                    return Err(usage(
                        "usage: kio manual export [--format=tar.gz-concat|tar|tar.gz]".into(),
                    ));
                }
            };
            if !matches!(format, "tar.gz-concat" | "tar" | "tar.gz") {
                return Err(usage(format!("unknown manual archive format {format:?}")));
            }
            if terminal {
                return Err(usage(
                    "refusing binary archive output to a terminal; pipe or redirect stdout".into(),
                ));
            }
            match format {
                "tar.gz-concat" => output.write_all(PAYLOAD).map_err(io_error),
                "tar" => tar(&entries, PAYLOAD, output).map_err(io_error),
                "tar.gz" => {
                    let mut gzip = flate2::GzBuilder::new()
                        .mtime(0)
                        .operating_system(255)
                        .write(output, flate2::Compression::default());
                    tar(&entries, PAYLOAD, &mut gzip).map_err(io_error)?;
                    gzip.finish().map_err(io_error)?;
                    Ok(())
                }
                _ => unreachable!("validated archive format"),
            }
        }
        _ => Err(usage(format!(
            "invalid manual operation or arguments; run kio manual --help (operation {operation:?})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn conventional_export_retains_long_asset_paths_and_binary_bytes() {
        let path = format!("docs/assets/{}.bin", "long-name-".repeat(20));
        let body = b"\0\xff\r\nasset\0";
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut source = tar::Builder::new(gzip);
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        source
            .append_data(&mut header, &path, body.as_slice())
            .unwrap();
        let payload = source.into_inner().unwrap().finish().unwrap();
        let entry = Entry {
            path: &path,
            size: body.len() as u64,
            offset: 0,
            length: payload.len(),
        };
        let mut copied = Vec::new();
        cat(&entry, &payload, &mut copied).unwrap();
        assert_eq!(copied, body);
        let mut output = Vec::new();
        tar(&[entry], &payload, &mut output).unwrap();
        let mut archive = tar::Archive::new(output.as_slice());
        let mut files = archive.entries().unwrap();
        let mut file = files.next().unwrap().unwrap();
        assert_eq!(file.path().unwrap().as_ref(), std::path::Path::new(&path));
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, body);
        drop(file);
        assert!(files.next().is_none());
    }

    fn invoke(args: &[&str]) -> Result<Vec<u8>, (i32, String)> {
        let mut output = Vec::new();
        execute(
            &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
            &mut output,
            false,
        )?;
        Ok(output)
    }

    #[test]
    fn listings_and_tree_match_the_index() {
        assert_eq!(
            invoke(&["ls"]).unwrap(),
            b"KIO-FOR-AGENTS.md\ndocs/\nspecs/\n"
        );
        let tree = String::from_utf8(invoke(&["tree"]).unwrap()).unwrap();
        let mut directories = Vec::new();
        let mut files = Vec::new();
        for line in tree.lines() {
            let depth = line.len() - line.trim_start_matches(' ').len();
            assert_eq!(depth % 2, 0);
            directories.truncate(depth / 2);
            let name = line.trim_start_matches(' ');
            if name.ends_with('/') {
                directories.push(name.trim_end_matches('/').to_owned());
            } else {
                files.push(
                    directories
                        .iter()
                        .map(String::as_str)
                        .chain([name])
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
        assert_eq!(
            files,
            entries().iter().map(|entry| entry.path).collect::<Vec<_>>()
        );
        assert!(
            invoke(&["tree", "docs/guides"])
                .unwrap()
                .starts_with(b"builtin-modules.md\n")
        );
    }

    #[test]
    fn listing_is_index_only_and_keeps_directory_children_together() {
        let fixture: Vec<_> = ["docs/a.md", "docs/a/file.md", "docs/z.md"]
            .into_iter()
            .map(|path| Entry {
                path,
                size: u64::MAX,
                offset: usize::MAX,
                length: usize::MAX,
            })
            .collect();
        let mut output = Vec::new();
        list(&fixture, "docs", true, &mut output).unwrap();
        assert_eq!(output, b"a.md\na/\n  file.md\nz.md\n");
    }

    #[test]
    fn selective_reads_reconstruct_the_original_content_identity() {
        let mut identity = blake3::Hasher::new();
        for entry in entries() {
            let bytes = invoke(&["cat", entry.path]).unwrap();
            assert_eq!(bytes.len() as u64, entry.size);
            identity.update(&(entry.path.len() as u64).to_le_bytes());
            identity.update(entry.path.as_bytes());
            identity.update(&entry.size.to_le_bytes());
            identity.update(&bytes);
        }
        assert_eq!(
            INDEX.lines().last().unwrap(),
            format!("identity\t{}", identity.finalize().to_hex())
        );
    }

    #[test]
    fn formats_have_identical_complete_contents_and_deterministic_bytes() {
        for format in ["tar.gz-concat", "tar", "tar.gz"] {
            let args = ["export", "--format", format];
            let bytes = invoke(&args).unwrap();
            assert_eq!(bytes, invoke(&args).unwrap());
            if format == "tar.gz-concat" {
                assert_eq!(bytes, PAYLOAD);
            }
            let decoded: Box<dyn Read> = if format == "tar" {
                Box::new(io::Cursor::new(bytes))
            } else {
                Box::new(flate2::read::MultiGzDecoder::new(io::Cursor::new(bytes)))
            };
            let mut archive = tar::Archive::new(decoded);
            archive.set_ignore_zeros(format == "tar.gz-concat");
            let mut paths = Vec::new();
            for file in archive.entries().unwrap() {
                let mut file = file.unwrap();
                let path = file.path().unwrap().to_str().unwrap().to_owned();
                assert_eq!(file.header().mode().unwrap(), 0o644);
                assert_eq!(file.header().mtime().unwrap(), 0);
                assert_eq!(file.header().uid().unwrap(), 0);
                assert_eq!(file.header().gid().unwrap(), 0);
                let mut body = Vec::new();
                file.read_to_end(&mut body).unwrap();
                assert_eq!(body, invoke(&["cat", &path]).unwrap());
                paths.push(path);
            }
            assert_eq!(
                paths,
                entries().iter().map(|entry| entry.path).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn invalid_arguments_and_terminal_refusal_write_no_payload() {
        for args in [
            vec!["cat"],
            vec!["cat", "docs"],
            vec!["ls", "KIO-FOR-AGENTS.md"],
            vec!["cat", "/docs/README.md"],
            vec!["cat", "docs/../README.md"],
            vec!["cat", "docs//README.md"],
            vec!["cat", "docs\\README.md"],
            vec!["cat", "docs/missing.md"],
            vec!["cat", "docs/README.md/"],
            vec!["search"],
            vec!["export", "--format=zip"],
            vec!["export", "extra"],
            vec!["tree", "docs", "extra"],
        ] {
            assert_eq!(invoke(&args).unwrap_err().0, 2, "{args:?}");
        }
        let mut output = Vec::new();
        assert_eq!(
            execute(&["export".into()], &mut output, true)
                .unwrap_err()
                .0,
            2
        );
        assert!(output.is_empty());
        execute(&["export".into(), "--help".into()], &mut output, true).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("kio manual export | tar --ignore-zeros -xzf -")
        );
    }

    #[test]
    fn output_errors_are_not_success() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected write failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for args in [
            vec!["ls"],
            vec!["tree"],
            vec!["cat", "docs/README.md"],
            vec!["export"],
            vec!["export", "--format=tar"],
            vec!["export", "--format=tar.gz"],
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert_eq!(execute(&args, &mut Broken, false).unwrap_err().0, 1);
        }
    }
}
