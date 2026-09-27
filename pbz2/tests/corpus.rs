mod common;

use std::path::{Path, PathBuf};

use common::{DECODE_WAYS, ENCODE_WAYS, decoded_by_bzip2, every_way};
use md5::{Digest, Md5};
use pbz2::Level;

fn corpora() -> [PathBuf; 5] {
    let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata");
    let bzip2_tests = std::env::var_os("BZIP2_TESTS_DIR")
        .map_or_else(|| testdata.join("bzip2-tests"), PathBuf::from);
    let folders = [
        bzip2_tests.join("commons-compress"),
        bzip2_tests.join("dotnetzip"),
        bzip2_tests.join("go"),
        testdata.join("other"),
        testdata.join("more"),
    ];
    for folder in &folders {
        assert!(
            folder.is_dir(),
            "the test corpus is missing at {}; run scripts/fetch-test-corpus.sh",
            folder.display()
        );
    }
    folders
}

struct Expected(String);

impl Expected {
    fn of(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path.with_extension("md5")).ok()?;
        text.split_whitespace()
            .next()
            .map(|md5| Self(md5.to_owned()))
    }

    fn matches(&self, bytes: &[u8]) -> Result<(), String> {
        let found = format!("{:x}", Md5::digest(bytes));
        if found == self.0 {
            Ok(())
        } else {
            Err(format!("md5 {found}, expected {}", self.0))
        }
    }
}

fn files_in(folder: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)
        .expect("the corpus folder can be read")
        .map(|entry| entry.expect("the entry can be read").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_in(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn corpus_files(ending: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for folder in corpora() {
        files_in(&folder, &mut found);
    }
    found.retain(|path| path.to_string_lossy().ends_with(ending));
    assert!(!found.is_empty(), "the corpus has no {ending} files");
    found
}

const TESTED_INPUT_BYTES: usize = 128 << 10;

fn small_good_files() -> Vec<(PathBuf, Vec<u8>, Expected)> {
    corpus_files(".bz2")
        .into_iter()
        .filter_map(|path| {
            let expected = Expected::of(&path)?;
            let input = std::fs::read(&path).expect("the file can be read");
            (input.len() <= TESTED_INPUT_BYTES).then_some((path, input, expected))
        })
        .collect()
}

#[test]
fn good_files_decode_to_their_md5() {
    let mut problems = Vec::new();
    for (path, input, expected) in small_good_files() {
        for (way, decode) in &DECODE_WAYS {
            if let Err(problem) = decode(&input).and_then(|out| expected.matches(&out)) {
                problems.push(format!("{}: {way}: {problem}", path.display()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn good_files_round_trip_through_libbzip2() {
    let mut problems = Vec::new();
    for (index, (path, input, expected)) in small_good_files().into_iter().enumerate() {
        let Ok(original) = decoded_by_bzip2(&input) else {
            continue;
        };
        let level = [Level::FASTEST, Level::BEST][index % 2];
        let (way, encode) = ENCODE_WAYS[index % ENCODE_WAYS.len()];
        let result = decoded_by_bzip2(&encode(&original, level))
            .map_err(|problem| problem.to_string())
            .and_then(|unpacked| expected.matches(&unpacked));
        if let Err(problem) = result {
            problems.push(format!(
                "{}: level {}: {way}: {problem}",
                path.display(),
                level.get()
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn bad_files_are_refused() {
    let mut problems = Vec::new();
    for path in corpus_files(".bz2.bad") {
        let input = std::fs::read(&path).expect("the file can be read");
        for (way, result) in every_way(&input) {
            if result.is_ok() {
                problems.push(format!("{}: {way}: accepted", path.display()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
