use std::fs::{self, File, FileTimes, Metadata, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, fchown};
use std::path::{Path, PathBuf};

use eyre::{Report, WrapErr, eyre};
use pbz2::{DecoderReader, EncoderWriter, Level, ParallelDecoder, ParallelEncoder};

use crate::names;

const BUFFER_BYTES: usize = 128 << 10;
const STDIN_NAME: &str = "(stdin)";
const SHORTEST_NAME_COLUMN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Compress,
    Decompress,
    Test,
}

#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) mode: Mode,
    pub(crate) to_stdout: bool,
    pub(crate) keep: bool,
    pub(crate) force: bool,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) level: Level,
    pub(crate) threads: usize,
    pub(crate) inputs: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Skipped(String);

fn skipped(message: String) -> Report {
    Report::new(Skipped(message))
}

fn io_errors(report: &Report) -> impl Iterator<Item = &io::Error> {
    report
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
}

fn exit_code(report: &Report) -> u8 {
    let damaged = io_errors(report).any(|error| {
        error
            .get_ref()
            .is_some_and(|inner| inner.is::<pbz2::pbz2_core::Error>())
    });
    if damaged { 2 } else { 1 }
}

fn is_broken_pipe(report: &Report) -> bool {
    io_errors(report).any(|error| error.kind() == io::ErrorKind::BrokenPipe)
}

fn copy_attributes(from: &Metadata, to: &File) -> io::Result<()> {
    #[cfg(unix)]
    let _ = fchown(to, Some(from.uid()), Some(from.gid()));
    to.set_permissions(from.permissions())?;
    to.set_times(
        FileTimes::new()
            .set_accessed(from.accessed()?)
            .set_modified(from.modified()?),
    )
}

fn decimal(numerator: u128, denominator: u128, places: u32) -> String {
    let scale = 10u128.pow(places);
    let scaled = numerator * scale;
    let mut units = scaled / denominator;
    let twice_remainder = (scaled % denominator) * 2;
    if twice_remainder > denominator || (twice_remainder == denominator && units & 1 == 1) {
        units += 1;
    }
    let width = places as usize;
    format!("{}.{:0width$}", units / scale, units % scale)
}

fn compression_summary(bytes_in: u64, bytes_out: u64) -> String {
    if bytes_in == 0 || bytes_out == 0 {
        return " no data compressed.".to_owned();
    }
    let (packed, original) = (u128::from(bytes_out), u128::from(bytes_in));
    let saved = if packed <= original {
        decimal(100 * (original - packed), original, 2)
    } else {
        format!("-{}", decimal(100 * (packed - original), original, 2))
    };
    format!(
        "{:>6}:1, {:>6} bits/byte, {saved:>5}% saved, {bytes_in} in, {bytes_out} out.",
        decimal(original, packed, 3),
        decimal(8 * packed, original, 3),
    )
}

struct Counted<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct Run<'a> {
    program: &'a str,
    options: &'a Options,
    name_column: usize,
}

pub(crate) fn run(program: &str, options: &Options) -> u8 {
    let standard_input = [PathBuf::from("-")];
    let inputs = if options.inputs.is_empty() {
        &standard_input[..]
    } else {
        &options.inputs
    };
    let run = Run {
        program,
        options,
        name_column: inputs
            .iter()
            .map(|input| display_name(input).chars().count())
            .fold(SHORTEST_NAME_COLUMN, usize::max),
    };
    let mut exit = 0;
    for input in inputs {
        let Err(report) = run.input(input) else {
            continue;
        };
        if is_broken_pipe(&report) {
            return exit;
        }
        if !(options.quiet && report.downcast_ref::<Skipped>().is_some()) {
            eprintln!("{program}: {report:#}");
        }
        exit = exit.max(exit_code(&report));
    }
    exit
}

fn file_size(metadata: &Metadata) -> Option<u64> {
    metadata.is_file().then_some(metadata.len())
}

fn display_name(input: &Path) -> String {
    if input == Path::new("-") {
        STDIN_NAME.to_owned()
    } else {
        input.display().to_string()
    }
}

impl Run<'_> {
    fn input(&self, input: &Path) -> Result<(), Report> {
        if input == Path::new("-") {
            let stdin = io::stdin();
            if self.options.mode != Mode::Compress && stdin.is_terminal() {
                return Err(eyre!("Compressed data can't be read from a terminal"));
            }
            return self.to_stream(stdin, STDIN_NAME, None);
        }
        let name = input.display().to_string();
        if self.options.mode == Mode::Compress
            && let Some(suffix) = names::compressed_suffix(input)
        {
            return Err(skipped(format!(
                "Input file {name} already has .{suffix} suffix"
            )));
        }
        let metadata =
            fs::metadata(input).wrap_err_with(|| format!("Can't open input file {name}"))?;
        if metadata.is_dir() {
            return Err(eyre!("Input file {name} is a directory"));
        }
        let to_file = !self.options.to_stdout && self.options.mode != Mode::Test;
        if to_file && !self.options.force {
            let link = fs::symlink_metadata(input)
                .wrap_err_with(|| format!("Can't open input file {name}"))?;
            if !link.is_file() {
                return Err(skipped(format!("Input file {name} is not a normal file")));
            }
        }
        let file = File::open(input).wrap_err_with(|| format!("Can't open input file {name}"))?;
        if !to_file {
            return self.to_stream(file, &name, file_size(&metadata));
        }
        let output = match self.options.mode {
            Mode::Compress => names::compressed(input),
            Mode::Decompress | Mode::Test => names::decompressed(input).unwrap_or_else(|| {
                let fallback = names::fallback(input);
                self.warn(&format!(
                    "Can't guess original name for {name}, using {}",
                    fallback.display()
                ));
                fallback
            }),
        };
        self.to_file(file, &metadata, &name, &output)?;
        if !self.options.keep {
            fs::remove_file(input).wrap_err_with(|| format!("Can't remove input file {name}"))?;
        }
        Ok(())
    }

    fn to_stream(
        &self,
        input: impl Read + Send + 'static,
        name: &str,
        size: Option<u64>,
    ) -> Result<(), Report> {
        match self.options.mode {
            Mode::Compress => {
                let stdout = io::stdout();
                if stdout.is_terminal() {
                    return Err(eyre!("Compressed data can't be written to a terminal"));
                }
                let sizes = self
                    .compress(input, stdout.lock(), size)
                    .wrap_err_with(|| name.to_owned())?;
                self.report_compressed(name, sizes);
            }
            Mode::Decompress => {
                let trailing = self
                    .decompress(input, io::stdout().lock())
                    .wrap_err_with(|| name.to_owned())?;
                self.report_decompressed(name, trailing, "done");
            }
            Mode::Test => {
                let trailing = self
                    .decompress(input, io::sink())
                    .wrap_err_with(|| name.to_owned())?;
                self.report_decompressed(name, trailing, "ok");
            }
        }
        Ok(())
    }

    fn to_file(
        &self,
        input: File,
        metadata: &Metadata,
        name: &str,
        output_path: &Path,
    ) -> Result<(), Report> {
        let output_name = output_path.display().to_string();
        let output = self.create(output_path, &output_name)?;
        let filled = self
            .fill(input, &output, name, file_size(metadata))
            .and_then(|()| {
                copy_attributes(metadata, &output)
                    .wrap_err_with(|| format!("Can't copy file attributes to {output_name}"))
            });
        if filled.is_err() {
            drop(output);
            let _ = fs::remove_file(output_path);
        }
        filled
    }

    fn fill(
        &self,
        input: File,
        output: &File,
        name: &str,
        size: Option<u64>,
    ) -> Result<(), Report> {
        if self.options.mode == Mode::Compress {
            let sizes = self
                .compress(input, output, size)
                .wrap_err_with(|| name.to_owned())?;
            self.report_compressed(name, sizes);
        } else {
            let trailing = self
                .decompress(input, output)
                .wrap_err_with(|| name.to_owned())?;
            self.report_decompressed(name, trailing, "done");
        }
        Ok(())
    }

    fn create(&self, path: &Path, name: &str) -> Result<File, Report> {
        if self.options.force
            && let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(Report::new(error).wrap_err(format!("Can't remove output file {name}")));
        }
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        open.open(path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                eyre!("Output file {name} already exists")
            } else {
                Report::new(error).wrap_err(format!("Can't create output file {name}"))
            }
        })
    }

    fn compress(
        &self,
        input: impl Read,
        output: impl Write,
        size: Option<u64>,
    ) -> io::Result<(u64, u64)> {
        let mut input = BufReader::with_capacity(BUFFER_BYTES, input);
        let mut output = Counted {
            inner: BufWriter::with_capacity(BUFFER_BYTES, output),
            bytes: 0,
        };
        let level = self.options.level;
        let bytes_in = if self.options.threads == 1 {
            let mut encoder = EncoderWriter::new(&mut output, level);
            let bytes = io::copy(&mut input, &mut encoder)?;
            encoder.finish()?;
            bytes
        } else {
            let mut encoder =
                ParallelEncoder::with_threads(&mut output, level, self.options.threads);
            if let Some(size) = size {
                encoder.expect_input_bytes(size);
            }
            let bytes = io::copy(&mut input, &mut encoder)?;
            encoder.finish()?;
            bytes
        };
        output.flush()?;
        Ok((bytes_in, output.bytes))
    }

    fn decompress(
        &self,
        input: impl Read + Send + 'static,
        output: impl Write,
    ) -> io::Result<bool> {
        if self.options.threads == 1 {
            let mut output = BufWriter::with_capacity(BUFFER_BYTES, output);
            let mut decoder = DecoderReader::new(input);
            io::copy(&mut decoder, &mut output)?;
            output.flush()?;
            return Ok(decoder.has_trailing_bytes());
        }
        let mut output = output;
        let mut decoder = ParallelDecoder::with_threads(input, self.options.threads);
        loop {
            let decoded = decoder.fill_buf()?;
            if decoded.is_empty() {
                break;
            }
            output.write_all(decoded)?;
            let count = decoded.len();
            decoder.consume(count);
        }
        output.flush()?;
        Ok(decoder.has_trailing_bytes())
    }

    fn warn(&self, message: &str) {
        if !self.options.quiet {
            eprintln!("{}: {message}", self.program);
        }
    }

    fn label(&self, name: &str) -> String {
        let padding = self.name_column.saturating_sub(name.chars().count());
        format!("  {name}: {:padding$}", "")
    }

    fn report_compressed(&self, name: &str, (bytes_in, bytes_out): (u64, u64)) {
        if self.options.verbose {
            eprintln!(
                "{}{}",
                self.label(name),
                compression_summary(bytes_in, bytes_out)
            );
        }
    }

    fn report_decompressed(&self, name: &str, trailing: bool, word: &str) {
        if trailing {
            self.warn(&format!(
                "{name}: ignored bytes after the end of the bzip2 data"
            ));
        }
        if self.options.verbose {
            eprintln!("{}{word}", self.label(name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_summary_matches_bzip2() {
        for (bytes_in, bytes_out, line) in [
            (
                98_696,
                32_348,
                " 3.051:1,  2.622 bits/byte, 67.22% saved, 98696 in, 32348 out.",
            ),
            (
                212_340,
                72_612,
                " 2.924:1,  2.736 bits/byte, 65.80% saved, 212340 in, 72612 out.",
            ),
            (
                2,
                39,
                " 0.051:1, 156.000 bits/byte, -1850.00% saved, 2 in, 39 out.",
            ),
            (0, 14, " no data compressed."),
        ] {
            assert_eq!(compression_summary(bytes_in, bytes_out), line);
        }
    }

    #[test]
    fn decimal_rounds_halves_to_even() {
        assert_eq!(decimal(1, 8, 2), "0.12");
        assert_eq!(decimal(3, 8, 2), "0.38");
        assert_eq!(decimal(1, 7, 3), "0.143");
    }
}
