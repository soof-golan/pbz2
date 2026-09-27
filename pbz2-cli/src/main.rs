#![forbid(unsafe_code)]

mod names;
mod run;

use std::ffi::OsStr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;

use lexopt::{Arg, Parser, ValueExt};
use pbz2::Level;

use crate::run::{Mode, Options};

const HELP: &str = "\
Parallel bzip2 compression and decompression

Usage: {program} [FLAGS] [FILE]...

  -z, --compress      Compress, the default
  -d, --decompress    Decompress
  -t, --test          Check that compressed files decompress, and write nothing
  -c, --stdout        Write to standard output and keep the input files
  -k, --keep          Keep the input files
  -f, --force         Overwrite output files, and read links and special files
  -q, --quiet         Hide warnings
  -v, --verbose       Report each file
  -s, --small         Accepted for bzip2 compatibility; has no effect
  -n, --threads N     Use N threads [default: one per core]
  -1, --fast          Fastest, with 100 kB blocks
  -9, --best          Smallest, with 900 kB blocks, the default
  -h, --help          Print this help
  -V, --version       Print the version

-2 to -8 pick block sizes between -1 and -9.
Run as bunzip2 it decompresses, and as bzcat it decompresses to standard output.
With no files, or with -, it reads standard input and writes standard output.
";

enum Command {
    Run(Options),
    Help,
    Version,
}

fn every_core() -> usize {
    thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

fn parse(program: &str, mut parser: Parser) -> Result<Command, lexopt::Error> {
    let runs_as_cat = ["zcat", "z2cat", "ZCAT", "Z2CAT"]
        .iter()
        .any(|name| program.contains(name));
    let runs_as_unzip = runs_as_cat || program.contains("unzip") || program.contains("UNZIP");
    let mut mode = None;
    let mut to_stdout = runs_as_cat;
    let (mut keep, mut force, mut quiet, mut verbose) = (false, false, false, false);
    let mut level = Level::BEST;
    let mut threads = None;
    let mut inputs = Vec::new();
    while let Some(argument) = parser.next()? {
        match argument {
            Arg::Short('z') | Arg::Long("compress") => mode = Some(Mode::Compress),
            Arg::Short('d') | Arg::Long("decompress") => mode = Some(Mode::Decompress),
            Arg::Short('t') | Arg::Long("test") => mode = Some(Mode::Test),
            Arg::Short('c') | Arg::Long("stdout") => to_stdout = true,
            Arg::Short('k') | Arg::Long("keep") => keep = true,
            Arg::Short('f') | Arg::Long("force") => force = true,
            Arg::Short('q') | Arg::Long("quiet") => quiet = true,
            Arg::Short('v') | Arg::Long("verbose") => verbose = true,
            Arg::Short('s') | Arg::Long("small") => {}
            Arg::Short('n') | Arg::Long("threads") => {
                threads = Some(parser.value()?.parse::<NonZeroUsize>()?.get());
            }
            Arg::Long("fast") => level = Level::FASTEST,
            Arg::Long("best") => level = Level::BEST,
            Arg::Short(digit @ '1'..='9') => {
                level = Level::new(digit as u8 - b'0').ok_or_else(|| argument.unexpected())?;
            }
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
            Arg::Short('V') | Arg::Long("version") => return Ok(Command::Version),
            Arg::Value(input) => inputs.push(PathBuf::from(input)),
            _ => return Err(argument.unexpected()),
        }
    }
    let default_mode = if runs_as_unzip {
        Mode::Decompress
    } else {
        Mode::Compress
    };
    Ok(Command::Run(Options {
        mode: mode.unwrap_or(default_mode),
        to_stdout,
        keep,
        force,
        quiet,
        verbose,
        level,
        threads: threads.unwrap_or_else(every_core),
        inputs,
    }))
}

fn main() -> ExitCode {
    let parser = Parser::from_env();
    let program = parser
        .bin_name()
        .map(Path::new)
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap_or("pbz2")
        .to_owned();
    match parse(&program, parser) {
        Ok(Command::Run(options)) => ExitCode::from(run::run(&program, &options)),
        Ok(Command::Help) => {
            print!("{}", HELP.replace("{program}", &program));
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("pbz2 {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{program}: {error}; run {program} --help for the flags");
            ExitCode::from(1)
        }
    }
}
