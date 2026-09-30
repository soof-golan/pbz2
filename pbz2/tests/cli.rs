use std::fs;
#[cfg(unix)]
use std::fs::{FileTimes, Permissions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
#[cfg(unix)]
use std::time::{Duration, SystemTime};

const PROGRAM: &str = env!("CARGO_BIN_EXE_pbz2");

struct Folder {
    path: PathBuf,
}

impl Folder {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("pbz2-command-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the folder can be made");
        Self { path }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.join(name);
        fs::write(&path, bytes).expect("the file can be written");
        path
    }

    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.join(name)).expect("the file can be read")
    }

    fn has(&self, name: &str) -> bool {
        self.join(name).exists()
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.run_with_input(Path::new(PROGRAM), arguments, &[])
    }

    fn run_with_input(&self, program: &Path, arguments: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(program)
            .args(arguments)
            .current_dir(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("pbz2 starts");
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input = input.to_vec();
        let feeder = thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let output = child.wait_with_output().expect("pbz2 runs");
        feeder.join().expect("the input is written");
        output
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn text() -> Vec<u8> {
    (0..30_000u32)
        .map(|line| format!("line {line} holds {}\n", line.wrapping_mul(line) % 9973))
        .collect::<String>()
        .into_bytes()
}

fn made_by_bzip2(data: &[u8]) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(9));
    encoder.write_all(data).expect("bzip2 compresses");
    encoder.finish().expect("bzip2 finishes")
}

fn decoded_by_bzip2(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    bzip2::read::MultiBzDecoder::new(input)
        .read_to_end(&mut out)
        .expect("bzip2 decodes it");
    out
}

fn exit_code(output: &Output) -> Option<i32> {
    output.status.code()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn compress_replaces_file_with_bz2() {
    let folder = Folder::new("compress-replaces");
    folder.write("a", &text());
    let output = folder.run(&["a"]);
    assert_eq!(exit_code(&output), Some(0), "{}", stderr(&output));
    assert!(!folder.has("a"));
    assert_eq!(decoded_by_bzip2(&folder.read("a.bz2")), text());
}

#[test]
fn decompress_replaces_bz2_with_original() {
    let folder = Folder::new("decompress-replaces");
    folder.write("a.bz2", &made_by_bzip2(&text()));
    let output = folder.run(&["-d", "a.bz2"]);
    assert_eq!(exit_code(&output), Some(0), "{}", stderr(&output));
    assert!(!folder.has("a.bz2"));
    assert_eq!(folder.read("a"), text());
}

#[test]
fn keep_leaves_input() {
    let folder = Folder::new("keep");
    folder.write("a", b"kept");
    assert_eq!(exit_code(&folder.run(&["-k", "a"])), Some(0));
    assert_eq!(folder.read("a"), b"kept");
    assert_eq!(decoded_by_bzip2(&folder.read("a.bz2")), b"kept");
}

#[cfg(unix)]
#[test]
fn mode_and_times_are_copied_both_ways() {
    let folder = Folder::new("attributes");
    let path = folder.write("a", b"attributes");
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_934_240);
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("the file opens")
        .set_times(FileTimes::new().set_modified(modified))
        .expect("the time is set");
    fs::set_permissions(&path, Permissions::from_mode(0o640)).expect("the mode is set");
    for (arguments, name) in [(&["a"][..], "a.bz2"), (&["-d", "a.bz2"], "a")] {
        assert_eq!(exit_code(&folder.run(arguments)), Some(0));
        let metadata = fs::metadata(folder.join(name)).expect("the output exists");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640, "{name}");
        assert_eq!(metadata.modified().ok(), Some(modified), "{name}");
    }
}

#[test]
fn decompress_names_match_bzip2_suffixes() {
    let folder = Folder::new("suffixes");
    let packed = made_by_bzip2(b"x");
    for name in ["one.bz2", "two.bz", "three.tbz2", "four.tbz", "five"] {
        folder.write(name, &packed);
    }
    let output = folder.run(&["-d", "one.bz2", "two.bz", "three.tbz2", "four.tbz", "five"]);
    assert_eq!(exit_code(&output), Some(0), "{}", stderr(&output));
    for name in ["one", "two", "three.tar", "four.tar", "five.out"] {
        assert_eq!(folder.read(name), b"x", "{name}");
    }
    assert!(stderr(&output).contains("five.out"));
}

#[test]
fn existing_output_is_not_overwritten() {
    let folder = Folder::new("existing");
    folder.write("a", b"new");
    folder.write("a.bz2", b"old");
    let output = folder.run(&["a"]);
    assert_eq!(exit_code(&output), Some(1));
    assert_eq!(folder.read("a.bz2"), b"old");
    assert_eq!(folder.read("a"), b"new");
}

#[test]
fn force_overwrites_existing_output() {
    let folder = Folder::new("force");
    folder.write("a", b"new");
    folder.write("a.bz2", b"old");
    assert_eq!(exit_code(&folder.run(&["-f", "a"])), Some(0));
    assert_eq!(decoded_by_bzip2(&folder.read("a.bz2")), b"new");
}

#[test]
fn compressed_suffix_is_skipped() {
    let folder = Folder::new("suffix-skipped");
    folder.write("a.bz2", b"plain");
    let output = folder.run(&["a.bz2"]);
    assert_eq!(exit_code(&output), Some(1));
    assert!(stderr(&output).contains("already has .bz2 suffix"));
    assert_eq!(folder.read("a.bz2"), b"plain");
    assert!(!folder.has("a.bz2.bz2"));
    let quiet = folder.run(&["-q", "a.bz2"]);
    assert_eq!(exit_code(&quiet), Some(1));
    assert_eq!(stderr(&quiet), "");
}

#[test]
fn directory_is_refused() {
    let folder = Folder::new("directory");
    fs::create_dir(folder.join("inside")).expect("the folder can be made");
    assert_eq!(exit_code(&folder.run(&["inside"])), Some(1));
}

#[test]
fn missing_file_is_refused() {
    let folder = Folder::new("missing");
    assert_eq!(exit_code(&folder.run(&["-d", "missing.bz2"])), Some(1));
}

#[cfg(unix)]
#[test]
fn symlink_is_refused_without_force() {
    let folder = Folder::new("symlink");
    folder.write("real.bz2", &made_by_bzip2(b"linked"));
    std::os::unix::fs::symlink("real.bz2", folder.join("link.bz2")).expect("the link is made");
    assert_eq!(exit_code(&folder.run(&["-d", "link.bz2"])), Some(1));
    assert!(!folder.has("link"));
    assert_eq!(exit_code(&folder.run(&["-dkf", "link.bz2"])), Some(0));
    assert_eq!(folder.read("link"), b"linked");
}

#[test]
fn truncated_file_is_refused_and_leaves_no_output() {
    let folder = Folder::new("truncated");
    let packed = made_by_bzip2(&text());
    folder.write("a.bz2", &packed[..packed.len() / 2]);
    let output = folder.run(&["-d", "a.bz2"]);
    assert_eq!(exit_code(&output), Some(2));
    assert!(!folder.has("a"));
    assert!(folder.has("a.bz2"));
}

#[test]
fn non_bzip2_file_is_refused() {
    let folder = Folder::new("not-bzip2");
    folder.write("a.bz2", b"plain text");
    let output = folder.run(&["-d", "a.bz2"]);
    assert_eq!(exit_code(&output), Some(2));
    assert!(!folder.has("a"));
}

#[test]
fn trailing_bytes_warn_and_succeed() {
    let folder = Folder::new("trailing");
    folder.write(
        "a.bz2",
        &[made_by_bzip2(b"data"), b"junk".to_vec()].concat(),
    );
    let output = folder.run(&["-d", "a.bz2"]);
    assert_eq!(exit_code(&output), Some(0));
    assert_eq!(folder.read("a"), b"data");
    assert_eq!(
        stderr(&output),
        "pbz2: a.bz2: ignored bytes after the end of the bzip2 data\n"
    );
    folder.write("b.bz2", &made_by_bzip2(b"data"));
    assert_eq!(stderr(&folder.run(&["-d", "b.bz2"])), "");
}

#[test]
fn standard_input_round_trips() {
    let folder = Folder::new("stdin");
    let program = Path::new(PROGRAM);
    let packed = folder.run_with_input(program, &[], &text());
    assert_eq!(exit_code(&packed), Some(0));
    assert_eq!(decoded_by_bzip2(&packed.stdout), text());
    let unpacked = folder.run_with_input(program, &["-d"], &made_by_bzip2(&text()));
    assert_eq!(exit_code(&unpacked), Some(0));
    assert_eq!(unpacked.stdout, text());
    let dash = folder.run_with_input(program, &["-dc", "-"], &made_by_bzip2(b"dash"));
    assert_eq!(dash.stdout, b"dash");
}

#[test]
fn stdout_flag_concatenates_files_and_keeps_them() {
    let folder = Folder::new("stdout");
    folder.write("a", b"first ");
    folder.write("b", b"second");
    let output = folder.run(&["-c", "a", "b"]);
    assert_eq!(exit_code(&output), Some(0));
    assert_eq!(decoded_by_bzip2(&output.stdout), b"first second");
    assert!(folder.has("a") && folder.has("b"));
    assert!(!folder.has("a.bz2"));
}

#[test]
fn errors_do_not_stop_later_files() {
    let folder = Folder::new("later-files");
    folder.write("bad.bz2", b"plain text");
    folder.write("good.bz2", &made_by_bzip2(b"good"));
    let output = folder.run(&["-dc", "bad.bz2", "missing.bz2", "good.bz2"]);
    assert_eq!(exit_code(&output), Some(2));
    assert_eq!(output.stdout, b"good");
}

#[test]
fn test_flag_checks_without_writing() {
    let folder = Folder::new("test-flag");
    folder.write("good.bz2", &made_by_bzip2(b"good"));
    folder.write("bad.bz2", b"plain text");
    let good = folder.run(&["-tv", "good.bz2"]);
    assert_eq!(exit_code(&good), Some(0));
    assert_eq!(stderr(&good), "  good.bz2: ok\n");
    assert_eq!(exit_code(&folder.run(&["-t", "bad.bz2"])), Some(2));
    assert!(!folder.has("good") && !folder.has("bad"));
    assert!(folder.has("good.bz2") && folder.has("bad.bz2"));
}

#[test]
fn level_flags_set_block_size_and_last_wins() {
    let folder = Folder::new("levels");
    folder.write("a", b"level");
    for (arguments, header) in [
        (&["-c", "a"][..], b"BZh9"),
        (&["-1", "-c", "a"], b"BZh1"),
        (&["-5c", "a"], b"BZh5"),
        (&["--fast", "-c", "a"], b"BZh1"),
        (&["--best", "-c", "a"], b"BZh9"),
        (&["-19", "-c", "a"], b"BZh9"),
        (&["-91", "-c", "a"], b"BZh1"),
        (&["--best", "--fast", "-c", "a"], b"BZh1"),
    ] {
        let output = folder.run(arguments);
        assert_eq!(output.stdout.get(..4), Some(&header[..]), "{arguments:?}");
        assert_eq!(decoded_by_bzip2(&output.stdout), b"level");
    }
}

#[test]
fn one_thread_round_trips() {
    let folder = Folder::new("one-thread");
    folder.write("a", &text());
    assert_eq!(exit_code(&folder.run(&["-n", "1", "a"])), Some(0));
    assert_eq!(decoded_by_bzip2(&folder.read("a.bz2")), text());
    assert_eq!(exit_code(&folder.run(&["-dn1", "a.bz2"])), Some(0));
    assert_eq!(folder.read("a"), text());
    assert_eq!(exit_code(&folder.run(&["-n", "0", "a"])), Some(1));
}

#[test]
fn unknown_flag_is_refused() {
    let folder = Folder::new("unknown-flag");
    assert_eq!(exit_code(&folder.run(&["-x"])), Some(1));
}

#[test]
fn verbose_decompress_matches_bzip2_format() {
    let folder = Folder::new("verbose");
    folder.write("q.bz2", &made_by_bzip2(b"q\n"));
    let output = folder.run(&["-dv", "q.bz2"]);
    assert_eq!(stderr(&output), "  q.bz2:   done\n");
}

#[test]
fn empty_file_round_trips() {
    let folder = Folder::new("empty");
    folder.write("empty", b"");
    assert_eq!(exit_code(&folder.run(&["empty"])), Some(0));
    assert_eq!(decoded_by_bzip2(&folder.read("empty.bz2")), b"");
    assert_eq!(exit_code(&folder.run(&["-d", "empty.bz2"])), Some(0));
    assert_eq!(folder.read("empty"), b"");
}

#[cfg(unix)]
#[test]
fn bunzip2_and_bzcat_names_decompress() {
    let folder = Folder::new("names");
    for name in ["bunzip2", "bzcat"] {
        std::os::unix::fs::symlink(PROGRAM, folder.join(name)).expect("the link is made");
    }
    folder.write("a.bz2", &made_by_bzip2(b"by name"));
    let cat = folder.run_with_input(&folder.join("bzcat"), &["a.bz2"], &[]);
    assert_eq!(exit_code(&cat), Some(0));
    assert_eq!(cat.stdout, b"by name");
    assert!(folder.has("a.bz2"));
    let unzip = folder.run_with_input(&folder.join("bunzip2"), &["a.bz2"], &[]);
    assert_eq!(exit_code(&unzip), Some(0));
    assert_eq!(folder.read("a"), b"by name");
    assert!(!folder.has("a.bz2"));
}
