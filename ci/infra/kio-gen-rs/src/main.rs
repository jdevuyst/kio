//! kio-gen CLI: generate a batch of Kio test cases.
//!
//! Usage:
//!   kio-gen --output <dir> [--seed <N>] [--count <N>] [-j <N>] [--prime-only]
//!
//! Default emission is "full Kio": Kio' core plus the surface forms
//! the generator has grown (`if!`/`else`, tuple literals, label-value
//! sugar, `match!`, the algebraic / spine elaborators, user-defined
//! `elab`, and operators). `--prime-only` is an opt-in filter that restricts
//! output to the Kio' core.

use std::path::PathBuf;
use std::process::ExitCode;

use rayon::prelude::*;

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use kio_gen::generate::GenOpts;
use kio_gen::{emit, generate, mutate, program_seed, shrink};

const HELP: &str = "\
Usage: kio-gen --output <dir> [options]

Generate a batch of Kio test cases compatible with ci/run-tests.sh.
Default emission is full Kio (Kio' core plus the surface forms the
generator has grown). Pass --prime-only to restrict output to Kio'.

Required:
  --output <dir>    write the batch under this directory

Options:
  --seed <N>             deterministic seed (default: random per run)
  --count <N>            number of programs to generate (default: 100)
  -j, --jobs <N>         rayon thread count (default: rayon's auto detect)
  --no-shrink            keep the full body around mutated programs
                         (default: shrink to a minimal repro)
  --prime-only           restrict emission to Kio' (skip surface forms)
  --valid-fraction <F>   share of the corpus that stays unmutated, in
                         [0.0, 1.0] (default: 0.2). 0.0 means every
                         program is mutated; 1.0 means none is. The
                         default targets ~uniform exposure per
                         coverable exit code (0 / 11 / 12 / 13 / 14):
                         20% success + 20% × 4 mutated.
  -h, --help             show this help and exit

Determinism: same --seed and --count produce the same batch regardless of -j.
";

struct Args {
    output: PathBuf,
    seed: Option<u64>,
    count: u64,
    jobs: Option<usize>,
    shrink: bool,
    valid_fraction: f64,
    prime_only: bool,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut output: Option<PathBuf> = None;
    let mut seed: Option<u64> = None;
    let mut count: u64 = 100;
    let mut jobs: Option<usize> = None;
    let mut shrink = true;
    let mut valid_fraction: f64 = 0.2;
    let mut prime_only = false;

    let mut i = 0;
    while i < raw.len() {
        let arg = &raw[i];
        let take_value = |i: &mut usize, flag: &str| -> Result<String, String> {
            *i += 1;
            raw.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--output" | "-o" => output = Some(take_value(&mut i, "--output")?.into()),
            "--seed" => {
                seed = Some(
                    take_value(&mut i, "--seed")?
                        .parse()
                        .map_err(|e| format!("--seed: {e}"))?,
                )
            }
            "--count" => {
                count = take_value(&mut i, "--count")?
                    .parse()
                    .map_err(|e| format!("--count: {e}"))?
            }
            "-j" | "--jobs" => {
                jobs = Some(
                    take_value(&mut i, "--jobs")?
                        .parse()
                        .map_err(|e| format!("--jobs: {e}"))?,
                )
            }
            "--no-shrink" => shrink = false,
            "--prime-only" => prime_only = true,
            "--valid-fraction" => {
                valid_fraction = take_value(&mut i, "--valid-fraction")?
                    .parse()
                    .map_err(|e| format!("--valid-fraction: {e}"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }

    let output = output.ok_or_else(|| "--output <dir> is required".to_string())?;
    if !(0.0..=1.0).contains(&valid_fraction) {
        return Err(format!(
            "--valid-fraction: {valid_fraction} is outside [0.0, 1.0]"
        ));
    }
    Ok(Args {
        output,
        seed,
        count,
        jobs,
        shrink,
        valid_fraction,
        prime_only,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kio-gen: {e}");
            return ExitCode::from(2);
        }
    };

    let seed = match args.seed {
        Some(s) => s,
        None => rand::random(),
    };
    let opts = GenOpts {
        prime_only: args.prime_only,
    };
    println!("kio-gen: seed = {seed}");
    println!("kio-gen: count = {}", args.count);
    println!("kio-gen: prime-only = {}", args.prime_only);
    println!("kio-gen: output = {}", args.output.display());

    if let Some(j) = args.jobs
        && let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(j)
            .build_global()
    {
        eprintln!("kio-gen: rayon: {e}");
        return ExitCode::from(1);
    }

    if let Err(e) = std::fs::create_dir_all(&args.output) {
        eprintln!("kio-gen: cannot create output dir: {e}");
        return ExitCode::from(1);
    }

    // The valid/mutated split per program is deterministic in the
    // seed alone, so --seed N reproduces the same batch. Default
    // valid_fraction is 0.2 — that targets roughly equal exposure
    // per coverable exit code (5 codes: 0, 11, 12, 13, 14). Use
    // --valid-fraction 0.0 / 1.0 to force all-mutated / all-valid.
    let result: Result<(), String> = (0..args.count)
        .into_par_iter()
        .map(|i| {
            let prog_seed = program_seed(seed, i);
            let prog = generate::program_from_seed_opts(prog_seed, &opts);
            let case_name = format!("prog_{i:06}");

            // A second RNG, derived from the same per-program seed,
            // decides valid-vs-mutated and (if mutated) which mutation.
            let mut decision = ChaCha20Rng::seed_from_u64(prog_seed ^ 0x4d75_7461_7465);
            let mutate_it = !decision.gen_bool(args.valid_fraction);

            if mutate_it {
                let m = mutate::pick(&mut decision);
                // Shrinking the body to a minimal expression doesn't
                // change which mutator fires — it just removes the
                // generator's incidental scaffolding around the bug.
                let to_mutate = if args.shrink {
                    shrink::minimize_body(&prog)
                } else {
                    prog.clone()
                };
                let (files, exit_code, category) = mutate::apply(&to_mutate, m);
                let is_kio_prime = !to_mutate.surface_mode && m.preserves_kio_prime_grammar();
                emit::write_case(
                    &args.output,
                    category,
                    &case_name,
                    &files,
                    exit_code,
                    is_kio_prime,
                    emit::runner_protocol(to_mutate.surface_mode),
                )
                .map_err(|e| format!("case {category}/{case_name}: {e}"))
            } else {
                emit::write_valid_case(&args.output, &case_name, &prog)
                    .map_err(|e| format!("case {case_name}: {e}"))
            }
        })
        .collect();

    match result {
        Ok(()) => {
            println!(
                "kio-gen: wrote {count} cases to {out}",
                count = args.count,
                out = args.output.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("kio-gen: {e}");
            ExitCode::from(1)
        }
    }
}
