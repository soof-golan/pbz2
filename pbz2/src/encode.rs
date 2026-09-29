use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};

use crate::decode::to_io;
use crate::engine::{Engine, Pipeline, Started, Starts, on_engine, start};
use crate::pool::{every_core, with_block, with_scratch};
use crate::workers::Workers;
use pbz2_core::{
    Backend, BlockSplitter, EncodedBlock, Encoder, Error, InputBlock, Level, Pulled, RawBlock,
    StreamAssembler, code_runs_with, encode_block_with, encode_scratch_words, encoded_block_bytes,
};

const OUT_BYTES: usize = 1 << 16;
const BLOCKS_WAITING_PER_THREAD: usize = 2;
const QUEUED_BYTES: usize = 1 << 20;
const STREAM_END_BYTES: usize = 16;
const RAW_BUFFER_BLOCKS: usize = 2;
const SPARE_INPUTS: usize = 2;
const SMALLEST_TAIL_BLOCK: u64 = 400_000;

fn finished_already() -> io::Error {
    io::Error::other("the bzip2 stream has already been ended")
}

struct Writing<W, B> {
    inner: W,
    encoder: Encoder<Vec<u8>, Vec<u32>, B>,
    out: Vec<u8>,
}

struct StartWriting<W> {
    inner: W,
    level: Level,
}

impl<W> Pipeline for StartWriting<W> {
    type Running<B: Engine> = Writing<W, B>;
}

impl<W> Starts<Self> for StartWriting<W> {
    fn start<B: Engine>(self, backend: B) -> Writing<W, B> {
        let block = self.level.block_bytes();
        let encoder = Encoder::with_backend(
            self.level,
            vec![0; block],
            vec![0; encode_scratch_words(block)],
            backend,
        )
        .expect("the buffers are sized for the level");
        Writing {
            inner: self.inner,
            encoder,
            out: vec![0; OUT_BYTES],
        }
    }
}

impl<W: Write, B: Backend> Writing<W, B> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        loop {
            let taken = self.encoder.push(input);
            if taken > 0 {
                return Ok(taken);
            }
            self.write_out()?;
        }
    }

    fn write_out(&mut self) -> io::Result<()> {
        loop {
            match self.encoder.pull(&mut self.out) {
                Pulled::Bytes(count) => self.inner.write_all(&self.out[..count])?,
                Pulled::NeedInput | Pulled::Finished => return Ok(()),
            }
        }
    }

    fn end(mut self) -> io::Result<W> {
        self.encoder.end_input();
        self.write_out()?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// Compresses data written to it on the calling thread and writes the bzip2 stream to
/// another writer.
///
/// Call [`EncoderWriter::finish`] to end the stream and get the writer back. Dropping it
/// also ends the stream, but ignores errors. `flush` only flushes the inner writer: bytes
/// written since the last full block stay in the encoder until the block fills up or the
/// stream ends.
pub struct EncoderWriter<W: Write> {
    writing: Option<Started<StartWriting<W>>>,
}

impl<W: Write> EncoderWriter<W> {
    /// Compresses into `inner` at `level`.
    pub fn new(inner: W, level: Level) -> Self {
        Self {
            writing: Some(start(StartWriting { inner, level })),
        }
    }

    fn writing(&mut self) -> io::Result<&mut Started<StartWriting<W>>> {
        self.writing.as_mut().ok_or_else(finished_already)
    }

    /// Ends the stream, flushes the writer, and returns it.
    ///
    /// # Errors
    ///
    /// Any error from writing.
    pub fn finish(mut self) -> io::Result<W> {
        let writing = self.writing.take().ok_or_else(finished_already)?;
        on_engine!(writing, writing => writing.end())
    }
}

impl<W: Write> Write for EncoderWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        on_engine!(self.writing()?, writing => writing.write(input))
    }

    fn flush(&mut self) -> io::Result<()> {
        on_engine!(self.writing()?, writing => writing.inner.flush())
    }
}

impl<W: Write> Drop for EncoderWriter<W> {
    fn drop(&mut self) {
        if let Some(writing) = self.writing.take() {
            let _ = on_engine!(writing, writing => writing.end());
        }
    }
}

struct Encoded {
    output: Vec<u8>,
    result: Result<EncodedBlock, Error>,
}

enum Job {
    Raw(RawBlock),
    Filled(InputBlock),
}

impl Job {
    const fn length(&self) -> usize {
        match self {
            Self::Raw(block) => block.length(),
            Self::Filled(block) => block.length(),
        }
    }
}

struct Task {
    input: Vec<u8>,
    out: Vec<u8>,
    job: Job,
    answer: SyncSender<Encoded>,
}

enum Returned {
    Started,
    Input(Vec<u8>),
}

fn encode<B: Backend>(
    task: Task,
    scratch_words: usize,
    give_back: &Sender<Returned>,
    backend: B,
) -> Encoded {
    let Task {
        mut input,
        mut out,
        job,
        ..
    } = task;
    let needed = encoded_block_bytes(job.length());
    if out.len() < needed {
        out = vec![0; needed];
    }
    let result = with_scratch(scratch_words, |scratch| match job {
        Job::Raw(raw) => with_block(raw.length(), |block| {
            let filled = code_runs_with(&input, raw, block, backend);
            let _ = give_back.send(Returned::Input(input));
            encode_block_with(block, filled?, scratch, &mut out, backend)
        }),
        Job::Filled(filled) => {
            let result = encode_block_with(&mut input, filled, scratch, &mut out, backend);
            let _ = give_back.send(Returned::Input(input));
            result
        }
    });
    Encoded {
        output: out,
        result,
    }
}

struct Running<W, B> {
    inner: W,
    splitter: BlockSplitter,
    assembler: StreamAssembler,
    raw: Vec<u8>,
    raw_bytes: usize,
    filling: Option<Vec<u8>>,
    spare_inputs: Vec<Vec<u8>>,
    spare_outputs: Vec<Vec<u8>>,
    workers: Workers<Task>,
    returned: Receiver<Returned>,
    not_started: usize,
    most_not_started: usize,
    waiting: VecDeque<Receiver<Encoded>>,
    most_waiting: usize,
    joined: Vec<u8>,
    threads: usize,
    remaining: Option<u64>,
    backend: B,
}

struct StartEncoding<W> {
    inner: W,
    level: Level,
    threads: usize,
}

impl<W> Pipeline for StartEncoding<W> {
    type Running<B: Engine> = Running<W, B>;
}

impl<W> Starts<Self> for StartEncoding<W> {
    fn start<B: Engine>(self, backend: B) -> Running<W, B> {
        let splitter = BlockSplitter::new(self.level);
        let block_bytes = splitter.block_bytes();
        let threads = self.threads.max(1);
        let raw_bytes = RAW_BUFFER_BLOCKS * block_bytes;
        let scratch_words = encode_scratch_words(block_bytes);
        let (give_back, returned) = channel();
        let workers = Workers::new(threads, move |task: Task| {
            let _ = give_back.send(Returned::Started);
            let answer = task.answer.clone();
            let _ = answer.send(encode(task, scratch_words, &give_back, backend));
        });
        Running {
            inner: self.inner,
            splitter,
            assembler: StreamAssembler::new(self.level),
            raw: Vec::with_capacity(raw_bytes),
            raw_bytes,
            filling: None,
            spare_inputs: Vec::new(),
            spare_outputs: Vec::new(),
            workers,
            returned,
            not_started: 0,
            most_not_started: (QUEUED_BYTES / block_bytes).min(threads).max(2),
            waiting: VecDeque::new(),
            most_waiting: threads * BLOCKS_WAITING_PER_THREAD,
            joined: vec![0; encoded_block_bytes(block_bytes) + STREAM_END_BYTES],
            threads,
            remaining: None,
            backend,
        }
    }
}

impl<W: Write, B: Engine> Running<W, B> {
    fn take_returned(&mut self, returned: Returned) {
        match returned {
            Returned::Started => self.not_started -= 1,
            Returned::Input(input) => {
                if self.spare_inputs.len() < SPARE_INPUTS {
                    self.spare_inputs.push(input);
                }
            }
        }
    }

    fn wait_for_room(&mut self) -> io::Result<()> {
        while let Ok(returned) = self.returned.try_recv() {
            self.take_returned(returned);
        }
        while self.not_started >= self.most_not_started {
            let returned = self
                .returned
                .recv()
                .map_err(|_| io::Error::other("a bzip2 encoder thread stopped"))?;
            self.take_returned(returned);
        }
        Ok(())
    }

    fn spare_input(&mut self) -> Vec<u8> {
        let mut spare = self
            .spare_inputs
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(self.raw_bytes));
        spare.clear();
        spare
    }

    fn send(&mut self, job: Job) -> io::Result<()> {
        self.wait_for_room()?;
        let input = match job {
            Job::Raw(_) => {
                let next = self.spare_input();
                std::mem::replace(&mut self.raw, next)
            }
            Job::Filled(_) => self.filling.take().expect("a block is being filled"),
        };
        let (byte, count) = self.splitter.pending_run();
        self.raw.clear();
        self.raw.resize(count, byte);
        let out = self.spare_outputs.pop().unwrap_or_default();
        let (answer, result) = sync_channel(1);
        self.workers.send(Task {
            input,
            out,
            job,
            answer,
        });
        self.not_started += 1;
        self.waiting.push_back(result);
        self.size_next_block();
        Ok(())
    }

    fn size_next_block(&mut self) {
        if let Some(remaining) = self.remaining {
            let share = (remaining / self.threads as u64).max(SMALLEST_TAIL_BLOCK);
            self.splitter
                .set_block_bytes(usize::try_from(share).unwrap_or(usize::MAX));
        }
    }

    fn expect_input_bytes(&mut self, bytes: u64) {
        self.remaining = Some(bytes);
        self.size_next_block();
    }

    fn fill_from_raw(&mut self) -> io::Result<()> {
        let mut block = self.spare_input();
        block.resize(self.splitter.block_bytes(), 0);
        self.splitter
            .fill_scanned_with(self.backend, &self.raw, &mut block)
            .map_err(to_io)?;
        self.raw.clear();
        self.filling = Some(block);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write_encoded(false)?;
        self.inner.flush()
    }

    fn write_encoded(&mut self, wait_for_all: bool) -> io::Result<()> {
        while let Some(front) = self.waiting.front() {
            let must_wait = wait_for_all || self.waiting.len() > self.most_waiting;
            let encoded = if must_wait {
                front.recv().ok()
            } else {
                match front.try_recv() {
                    Ok(encoded) => Some(encoded),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => None,
                }
            };
            let encoded =
                encoded.ok_or_else(|| io::Error::other("a bzip2 encoder thread stopped"))?;
            self.waiting.pop_front();
            let block = encoded.result.map_err(to_io)?;
            let count = self
                .assembler
                .append(
                    &encoded.output[..block.byte_length()],
                    block,
                    &mut self.joined,
                )
                .map_err(to_io)?;
            self.inner.write_all(&self.joined[..count])?;
            self.spare_outputs.push(encoded.output);
        }
        Ok(())
    }

    fn write(&mut self, mut input: &[u8]) -> io::Result<()> {
        while !input.is_empty() {
            let (taken, full) = if let Some(block) = &mut self.filling {
                let (taken, full) = self
                    .splitter
                    .fill_with(self.backend, input, block)
                    .map_err(to_io)?;
                (taken, full.map(Job::Filled))
            } else if self.raw.len() < self.raw_bytes {
                let room = self.raw_bytes - self.raw.len();
                let (taken, full) = self
                    .splitter
                    .scan_with(self.backend, &input[..input.len().min(room)]);
                self.raw.extend_from_slice(&input[..taken]);
                (taken, full.map(Job::Raw))
            } else {
                self.fill_from_raw()?;
                continue;
            };
            input = &input[taken..];
            if let Some(remaining) = &mut self.remaining {
                *remaining = remaining.saturating_sub(taken as u64);
            }
            if let Some(full) = full {
                self.send(full)?;
                self.write_encoded(false)?;
            }
        }
        Ok(())
    }

    fn end(mut self) -> io::Result<W> {
        loop {
            let last = match &mut self.filling {
                Some(block) => self.splitter.finish(block).map_err(to_io)?.map(Job::Filled),
                None => self.splitter.finish_scan().map(Job::Raw),
            };
            let Some(last) = last else { break };
            self.send(last)?;
        }
        self.write_encoded(true)?;
        let count = self.assembler.finish(&mut self.joined).map_err(to_io)?;
        self.inner.write_all(&self.joined[..count])?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// Compresses data written to it on every core and writes the bzip2 stream, in order, to
/// another writer on the calling thread.
///
/// Call [`ParallelEncoder::finish`] to end the stream and get the writer back. Dropping
/// it also ends the stream, but ignores errors. `flush` writes out the blocks compressed
/// so far and flushes the inner writer; it does not end the current block.
pub struct ParallelEncoder<W: Write> {
    running: Option<Started<StartEncoding<W>>>,
}

impl<W: Write> ParallelEncoder<W> {
    /// Compresses into `inner` at `level` on as many threads as there are cores.
    ///
    /// # Panics
    ///
    /// If the operating system cannot start the threads.
    pub fn new(inner: W, level: Level) -> Self {
        Self::with_threads(inner, level, every_core())
    }

    /// Compresses into `inner` at `level` on `threads` threads.
    ///
    /// # Panics
    ///
    /// If the operating system cannot start the threads.
    pub fn with_threads(inner: W, level: Level, threads: usize) -> Self {
        Self {
            running: Some(start(StartEncoding {
                inner,
                level,
                threads,
            })),
        }
    }

    fn running(&mut self) -> io::Result<&mut Started<StartEncoding<W>>> {
        self.running.as_mut().ok_or_else(finished_already)
    }

    /// Says that `bytes` more bytes will be written. Near the end of that input, blocks
    /// get smaller so every thread finishes at about the same time, down to 400 KB; the
    /// stream is slightly larger and its bytes depend on the thread count. Input that
    /// turns out longer or shorter is still compressed correctly.
    pub fn expect_input_bytes(&mut self, bytes: u64) {
        if let Some(running) = self.running.as_mut() {
            on_engine!(running, running => running.expect_input_bytes(bytes));
        }
    }

    /// Ends the stream, flushes the writer, and returns it.
    ///
    /// # Errors
    ///
    /// Any error from writing.
    pub fn finish(mut self) -> io::Result<W> {
        let running = self.running.take().ok_or_else(finished_already)?;
        on_engine!(running, running => running.end())
    }
}

impl<W: Write> Write for ParallelEncoder<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        on_engine!(self.running()?, running => running.write(input))?;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        on_engine!(self.running()?, running => running.flush())
    }
}

impl<W: Write> Drop for ParallelEncoder<W> {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = on_engine!(running, running => running.end());
        }
    }
}

/// Compresses all of `data` at `level` on every core.
///
/// # Panics
///
/// If a compression thread panics, which is a bug.
#[must_use]
pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
    let mut encoder = ParallelEncoder::new(Vec::new(), level);
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .expect("compressing into memory does not fail")
}
