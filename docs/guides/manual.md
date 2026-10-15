# Reading the bundled manual

Your installed `kio` includes documentation matching that compiler version.
You do not need a Kio source checkout or an internet connection to read it,
and these commands work from any directory:

```sh
kio manual
kio manual cat docs/README.md
kio manual cat KIO-FOR-AGENTS.md
kio manual cat TERMINOLOGY.md
```

Start with the documentation catalog for tutorials and topic guides. The
portable agent instructions help an AI assistant navigate the same material.
The terminology reference explains Kio-specific terms and distinctions.
For precise language rules, read `kio manual cat specs/language.md`.

## Discovering and reading files

```sh
kio manual ls
kio manual ls docs/guides
kio manual tree
kio manual tree docs/guides
kio manual cat docs/guides/products.md
```

`ls` lists a directory's immediate children. `tree` shows its complete subtree,
with indentation and `/` after directory names. Their default is the virtual
root containing `KIO-FOR-AGENTS.md`, `TERMINOLOGY.md`, `docs/`, and `specs/`.

`cat` writes the complete file exactly as bundled. Save a single file with
redirection, for example `kio manual cat specs/language.md > language.md`.
Paths use `/` and name bundled files, not files in your project. Resolve a
document's relative links within this tree and open the destination with
`kio manual cat`. Website and external repository links still need network
access; exporting the manual does not include linked packages or other files
outside this bundle inventory.

The bundled files are their original Markdown and assets, rather than rendered
website pages. Kiodoc fence attributes and hidden harnesses remain visible in
the source; the surrounding prose and visible examples are the reading path.

## Exporting for local browsing and search

Export the complete tree to use your preferred editor or search tool. Archive
contents match `kio manual tree`, with no extra internal metadata files.

The default `tar.gz-concat` format copies the stored compressed archives
directly. Each file is stored in a complete single-file tar.gz archive; the
export concatenates those archives. Extract into the current directory with:

```sh
kio manual export | tar --ignore-zeros -xzf -
```

`--ignore-zeros` reads past each inner tar archive's end marker. Without it,
extraction may succeed but produce only the first file. The final `-` tells
tar to read from standard input, so no intermediate archive file is needed.
Use a reader supporting the option and concatenated gzip members.

For immediate local extraction with ordinary tar options and no recompression,
choose a conventional uncompressed tar archive:

```sh
kio manual export --format=tar | tar -xf -
```

Choose `tar.gz` when you need a conventional compressed archive:

```sh
kio manual export --format=tar.gz | tar -xzf -
```

All three formats extract the same original paths and bytes. These choices
describe the processing involved, not a guaranteed speed difference. To save
an archive for later, use:

```sh
kio manual export > kio-manual.tar.gz-concat
tar --ignore-zeros -xzf kio-manual.tar.gz-concat
kio manual export --format=tar > kio-manual.tar
tar -xf kio-manual.tar
kio manual export --format=tar.gz > kio-manual.tar.gz
tar -xzf kio-manual.tar.gz
```

The `.tar.gz-concat` suffix distinguishes the default from conventional
tar.gz; it does not make a reader ignore inner end markers. `kio manual export`
refuses to write binary archive data directly to a terminal. Read
`kio manual export --help` for the format overview and extraction commands.
