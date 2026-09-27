use std::collections::VecDeque;
use std::io::{self, BufRead, Cursor, Read};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::thread;

use pbz2_core::{
    Backend, Decoder, Error, MAX_COMPRESSED_BLOCK_BYTES, Pulled, SCRATCH_WORDS, StreamChecker,
};
use rayon::ThreadPool;

use crate::engine::{Engine, Pipeline, Started, Starts, on_engine, start};
use crate::pool::{every_core, pool, with_scratch};
use crate::split::{Decoded, Kind, Segment, Spares, Splitter};

const READ_BYTES: usize = 1 << 20;
const FIRST_INPUT_BYTES: usize = 256 << 10;
const BLOCKS_WAITING_PER_THREAD: usize = 2;

pub(crate) fn to_io(problem: Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, problem)
}

type Answer = Receiver<(Segment, Result<Decoded, Error>)>;

pub(crate) enum Job {
    Waiting(Segment),
    Decoding(Answer),
    Decoded(Segment, Result<Decoded, Error>),
}

impl Job {
    fn kind(&self) -> Kind {
        match self {
            Self::Waiting(segment) | Self::Decoded(segment, _) => segment.kind,
            Self::Decoding(_) => Kind::Block,
        }
    }

    fn into_parts(self) -> (Segment, Option<Result<Decoded, Error>>) {
        match self {
            Self::Waiting(segment) => (segment, None),
            Self::Decoded(segment, result) => (segment, Some(result)),
            Self::Decoding(answer) => {
                let (segment, result) = answer
                    .recv()
                    .expect("a decoding task always answers, and a panic aborts");
                (segment, Some(result))
            }
        }
    }

    fn from_parts(segment: Segment, result: Option<Result<Decoded, Error>>) -> Self {
        match result {
            Some(result) => Self::Decoded(segment, result),
            None => Self::Waiting(segment),
        }
    }
}

#[derive(Clone, Copy)]
enum Joining {
    Open,
    Ended { trailing_bytes: bool },
}

enum Joined {
    Block(Decoded, u64),
    End { trailing_bytes: bool },
}

pub(crate) struct Joiner<Jobs, B> {
    jobs: Jobs,
    returned: VecDeque<Job>,
    checker: StreamChecker,
    scratch: Vec<u32>,
    joining: Joining,
    reading: Option<(Decoded, u64)>,
    spares: Spares,
    backend: B,
}

impl<Jobs: Iterator<Item = Job>, B: Backend> Joiner<Jobs, B> {
    pub(crate) fn new(jobs: Jobs, spares: Spares, backend: B) -> Self {
        Self {
            jobs,
            returned: VecDeque::new(),
            checker: StreamChecker::new(),
            scratch: Vec::new(),
            joining: Joining::Open,
            reading: None,
            spares,
            backend,
        }
    }

    pub(crate) const fn has_trailing_bytes(&self) -> bool {
        matches!(
            self.joining,
            Joining::Ended {
                trailing_bytes: true
            }
        )
    }

    pub(crate) const fn jobs_mut(&mut self) -> &mut Jobs {
        &mut self.jobs
    }

    pub(crate) fn fill(&mut self) -> Result<(), Error> {
        loop {
            if let Some((block, end_bit)) = &mut self.reading {
                if block.refill() {
                    return Ok(());
                }
                self.checker.block(&block.output, *end_bit)?;
                if let Some((block, _)) = self.reading.take() {
                    self.spares.give(block.into_bytes());
                }
            }
            if let Joining::Ended { .. } = self.joining {
                return Ok(());
            }
            match self.join_next()? {
                Joined::Block(block, end_bit) => self.reading = Some((block, end_bit)),
                Joined::End { trailing_bytes } => {
                    self.joining = Joining::Ended { trailing_bytes };
                    self.returned.clear();
                    for _ in self.jobs.by_ref() {}
                    return Ok(());
                }
            }
        }
    }

    pub(crate) fn filled(&self) -> &[u8] {
        self.reading
            .as_ref()
            .map_or(&[][..], |(block, _)| block.bytes())
    }

    pub(crate) fn consume(&mut self, count: usize) {
        if let Some((block, _)) = &mut self.reading {
            block.consume(count);
        }
    }

    fn join_next(&mut self) -> Result<Joined, Error> {
        loop {
            let Some(job) = self.next_job() else {
                self.checker.finish()?;
                return Ok(Joined::End {
                    trailing_bytes: self.checker.is_finished(),
                });
            };
            if self.checker.is_finished() {
                return Ok(Joined::End {
                    trailing_bytes: true,
                });
            }
            match job.kind() {
                Kind::Block if !self.checker.is_in_stream() => {
                    return Ok(Joined::End {
                        trailing_bytes: true,
                    });
                }
                Kind::Block => {
                    let (decoded, end_bit) = self.unpack(job)?;
                    return Ok(Joined::Block(decoded, end_bit));
                }
                Kind::Start => {
                    let (segment, _) = job.into_parts();
                    self.checker.start(&segment.bytes, segment.end_bit())?;
                }
                Kind::End => {
                    let (segment, _) = job.into_parts();
                    self.checker
                        .end(&segment.bytes, segment.first_bit, segment.end_bit())?;
                }
            }
        }
    }

    fn scratch(&mut self) -> &mut [u32] {
        if self.scratch.is_empty() {
            self.scratch = vec![0; SCRATCH_WORDS];
        }
        &mut self.scratch
    }

    fn unpack(&mut self, job: Job) -> Result<(Decoded, u64), Error> {
        let (segment, result) = job.into_parts();
        let first_try = match result {
            Some(result) => result,
            None => {
                let (spares, backend) = (self.spares.clone(), self.backend);
                segment.decode(self.scratch(), &spares, backend)
            }
        };
        match first_try {
            Ok(decoded) => return Ok((decoded, segment.end_bit())),
            Err(Error::Truncated) => {}
            Err(problem) => return Err(problem),
        }
        let end_bit = segment.end_bit();
        let mut joined = segment;
        let mut taken: Vec<(Segment, Option<Result<Decoded, Error>>)> = Vec::new();
        let mut tried = joined.bit_length;
        loop {
            let next = self.next_job().map(Job::into_parts);
            let last_try = next.is_none() || joined.bytes.len() > MAX_COMPRESSED_BLOCK_BYTES + 16;
            if let Some(next) = next {
                joined.join(&next.0);
                taken.push(next);
            }
            if !last_try && joined.bit_length < 2 * tried {
                continue;
            }
            tried = joined.bit_length;
            let (spares, backend) = (self.spares.clone(), self.backend);
            match joined.decode(self.scratch(), &spares, backend) {
                Ok(decoded) => {
                    let mut end_bit = end_bit;
                    let mut kept = 0;
                    while end_bit < decoded.output.end_bit() && kept < taken.len() {
                        end_bit += taken[kept].0.bit_length;
                        kept += 1;
                    }
                    for (segment, result) in taken.drain(kept..).rev() {
                        self.returned.push_front(Job::from_parts(segment, result));
                    }
                    return Ok((decoded, end_bit));
                }
                Err(Error::Truncated) if !last_try => {}
                Err(problem) => return Err(problem),
            }
        }
    }

    fn next_job(&mut self) -> Option<Job> {
        self.returned.pop_front().or_else(|| self.jobs.next())
    }
}

struct Arriving {
    jobs: Receiver<io::Result<Job>>,
    credits: SyncSender<()>,
    problem: Option<io::Error>,
}

impl Iterator for Arriving {
    type Item = Job;

    fn next(&mut self) -> Option<Job> {
        if self.problem.is_some() {
            return None;
        }
        match self.jobs.recv().ok()? {
            Ok(job) => {
                if job.kind() == Kind::Block {
                    let _ = self.credits.try_send(());
                }
                Some(job)
            }
            Err(problem) => {
                self.problem = Some(problem);
                None
            }
        }
    }
}

fn job_for<B: Engine>(segment: Segment, pool: &ThreadPool, spares: &Spares, backend: B) -> Job {
    if segment.kind != Kind::Block {
        return Job::Waiting(segment);
    }
    let (answer, result) = sync_channel(1);
    let spares = spares.clone();
    pool.spawn(move || {
        let decoded = with_scratch(SCRATCH_WORDS, |scratch| {
            segment.decode(scratch, &spares, backend)
        });
        let _ = answer.send((segment, decoded));
    });
    Job::Decoding(result)
}

struct Sending<'a> {
    pool: &'a ThreadPool,
    spares: &'a Spares,
    jobs: &'a Sender<io::Result<Job>>,
    credits: &'a Receiver<()>,
}

fn send_jobs<B: Engine>(segments: &mut Vec<Segment>, sending: &Sending<'_>, backend: B) -> bool {
    segments.drain(..).all(|segment| {
        if segment.kind == Kind::Block && sending.credits.recv().is_err() {
            return false;
        }
        let job = job_for(segment, sending.pool, sending.spares, backend);
        sending.jobs.send(Ok(job)).is_ok()
    })
}

fn split_into_jobs<B: Engine>(input: &mut impl Read, sending: &Sending<'_>, backend: B) {
    let jobs = sending.jobs;
    let mut splitter = Splitter::new();
    let mut segments = Vec::new();
    let mut buffer = vec![0u8; READ_BYTES];
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(problem) if problem.kind() == io::ErrorKind::Interrupted => continue,
            Err(problem) => {
                let _ = jobs.send(Err(problem));
                return;
            }
        };
        if let Err(problem) = splitter.push(&buffer[..read], &mut segments) {
            let _ = jobs.send(Err(to_io(problem)));
            return;
        }
        if !send_jobs(&mut segments, sending, backend) {
            return;
        }
    }
    splitter.finish(&mut segments);
    send_jobs(&mut segments, sending, backend);
}

struct Decoding;

impl Pipeline for Decoding {
    type Running<B: Engine> = Joiner<Arriving, B>;
}

struct StartDecoding<R> {
    input: R,
    threads: usize,
}

impl<R: Read + Send + 'static> Starts<Decoding> for StartDecoding<R> {
    fn start<B: Engine>(self, backend: B) -> Joiner<Arriving, B> {
        let threads = self.threads.max(1);
        let pool = pool(threads);
        let (jobs, arriving) = channel();
        let blocks_waiting = threads * BLOCKS_WAITING_PER_THREAD;
        let (credit, credits) = sync_channel(blocks_waiting);
        for _ in 0..blocks_waiting {
            let _ = credit.try_send(());
        }
        let mut input = self.input;
        let spares = Spares::default();
        let reader_spares = spares.clone();
        thread::spawn(move || {
            let sending = Sending {
                pool: &pool,
                spares: &reader_spares,
                jobs: &jobs,
                credits: &credits,
            };
            split_into_jobs(&mut input, &sending, backend);
        });
        Joiner::new(
            Arriving {
                jobs: arriving,
                credits: credit,
                problem: None,
            },
            spares,
            backend,
        )
    }
}

/// Decompresses bzip2 data from a reader on every core, and reads out in order.
///
/// Blocks are decoded as soon as their data arrives, so it works on data that is still
/// being received, such as a download.
pub struct ParallelDecoder {
    joiner: Started<Decoding>,
}

impl ParallelDecoder {
    /// Starts decoding `input` on as many threads as there are cores.
    ///
    /// # Panics
    ///
    /// If the operating system cannot start the threads.
    pub fn new(input: impl Read + Send + 'static) -> Self {
        Self::with_threads(input, every_core())
    }

    /// Starts decoding `input` on `threads` threads, plus one that reads `input`.
    ///
    /// # Panics
    ///
    /// If the operating system cannot start the threads.
    pub fn with_threads(input: impl Read + Send + 'static, threads: usize) -> Self {
        Self {
            joiner: start(StartDecoding { input, threads }),
        }
    }

    /// Whether the data ended with bytes after the last stream that are not another
    /// stream. They are ignored, and the bzip2 tool warns about them. Ask once reading has
    /// returned 0.
    #[must_use]
    pub fn has_trailing_bytes(&self) -> bool {
        on_engine!(&self.joiner, joiner => joiner.has_trailing_bytes())
    }
}

impl Read for ParallelDecoder {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let available = self.fill_buf()?;
        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for ParallelDecoder {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        on_engine!(&mut self.joiner, joiner => {
            let filled = joiner.fill();
            if let Some(problem) = joiner.jobs_mut().problem.take() {
                return Err(problem);
            }
            filled.map_err(to_io)?;
            Ok(joiner.filled())
        })
    }

    fn consume(&mut self, count: usize) {
        on_engine!(&mut self.joiner, joiner => joiner.consume(count));
    }
}

struct Reading<R, B> {
    input: R,
    decoder: Decoder<Vec<u8>, Vec<u32>, B>,
}

struct StartReading<R>(R);

impl<R> Pipeline for StartReading<R> {
    type Running<B: Engine> = Reading<R, B>;
}

impl<R> Starts<Self> for StartReading<R> {
    fn start<B: Engine>(self, backend: B) -> Reading<R, B> {
        Reading {
            input: self.0,
            decoder: Decoder::with_backend(
                vec![0; FIRST_INPUT_BYTES],
                vec![0; SCRATCH_WORDS],
                backend,
            ),
        }
    }
}

/// Decompresses bzip2 data from a reader on the calling thread.
///
/// Its input buffer starts at 256 KB and doubles, up to the largest block bzip2 allows,
/// only when a block does not fit.
pub struct DecoderReader<R> {
    reading: Started<StartReading<R>>,
}

impl<R: Read> DecoderReader<R> {
    /// Decodes `input`.
    pub fn new(input: R) -> Self {
        Self {
            reading: start(StartReading(input)),
        }
    }

    /// Whether the data ended with bytes after the last stream that are not another
    /// stream. They are ignored, and the bzip2 tool warns about them. Ask once reading has
    /// returned 0.
    #[must_use]
    pub fn has_trailing_bytes(&self) -> bool {
        on_engine!(&self.reading, reading => reading.decoder.has_trailing_bytes())
    }
}

impl<R: Read> Read for DecoderReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        on_engine!(&mut self.reading, reading => reading.read(out))
    }
}

impl<R: Read, B: Backend> Reading<R, B> {
    fn grow_input(&mut self) -> io::Result<()> {
        let bigger = (self.decoder.buffer_len() * 2).min(MAX_COMPRESSED_BLOCK_BYTES);
        self.decoder.grow_buffer(vec![0; bigger]).map_err(to_io)?;
        Ok(())
    }

    fn read_input(&mut self) -> io::Result<()> {
        let spare = self.decoder.spare_input();
        let read = loop {
            match self.input.read(spare) {
                Err(problem) if problem.kind() == io::ErrorKind::Interrupted => {}
                other => break other?,
            }
        };
        if read == 0 {
            self.decoder.end_input();
        } else {
            self.decoder.commit_input(read);
        }
        Ok(())
    }

    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.decoder.pull(out) {
                Ok(Pulled::Bytes(0)) => {}
                Ok(Pulled::Bytes(count)) => return Ok(count),
                Ok(Pulled::Finished) => return Ok(0),
                Ok(Pulled::NeedInput) => self.read_input()?,
                Err(Error::BufferTooSmall)
                    if self.decoder.buffer_len() < MAX_COMPRESSED_BLOCK_BYTES =>
                {
                    self.grow_input()?;
                }
                Err(problem) => return Err(to_io(problem)),
            }
        }
    }
}

/// Decompresses all of `data` on every core.
///
/// # Errors
///
/// [`crate::Error::Data`] if the data is damaged or is not bzip2.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, crate::Error> {
    let mut out = Vec::new();
    ParallelDecoder::new(Cursor::new(data.to_vec())).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn text_of_lines(count: u64) -> Vec<u8> {
        (0..count)
            .map(|line| format!("line {line} holds {}\n", line.wrapping_mul(line) % 9973))
            .collect::<String>()
            .into_bytes()
    }

    fn packed(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
        encoder.write_all(data).expect("the data can be packed");
        encoder.finish().expect("the packing finishes")
    }

    fn segments_of(packed: &[u8]) -> Vec<Segment> {
        let mut splitter = Splitter::new();
        let mut segments = Vec::new();
        for piece in packed.chunks(333) {
            splitter.push(piece, &mut segments).expect("it splits");
        }
        splitter.finish(&mut segments);
        segments
    }

    fn joined(segments: Vec<Segment>) -> Result<Vec<u8>, Error> {
        let jobs = segments.into_iter().map(Job::Waiting);
        let mut joiner = Joiner::new(jobs, Spares::default(), pbz2_core::native());
        let mut out = Vec::new();
        loop {
            joiner.fill()?;
            let count = joiner.filled().len().min(4096);
            if count == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&joiner.filled()[..count]);
            joiner.consume(count);
        }
    }

    #[test]
    fn false_block_marker_is_skipped() {
        let original = text_of_lines(40_000);
        let segments = segments_of(&packed(&original, 1));
        let late = segments[1].bit_length - 100;
        for bit in [1, 7, 48, 12_345, late] {
            let mut with_marker = segments.clone();
            let (before, after) = with_marker[1].split_at(bit);
            with_marker.splice(1..=1, [before, after]);
            assert_eq!(
                joined(with_marker).ok(),
                Some(original.clone()),
                "false marker at bit {bit}"
            );
        }
    }
}
