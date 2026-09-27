use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::thread;

use rayon::{ThreadPool, ThreadPoolBuilder};

thread_local! {
    static SCRATCH: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static BLOCK: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn with_block<R>(bytes: usize, work: impl FnOnce(&mut [u8]) -> R) -> R {
    BLOCK.with_borrow_mut(|block| {
        if block.len() < bytes {
            block.resize(bytes, 0);
        }
        work(&mut block[..bytes])
    })
}

pub(crate) fn with_scratch<R>(words: usize, work: impl FnOnce(&mut [u32]) -> R) -> R {
    SCRATCH.with_borrow_mut(|scratch| {
        if scratch.len() < words {
            scratch.resize(words, 0);
        }
        work(&mut scratch[..words])
    })
}

pub(crate) fn every_core() -> usize {
    thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

pub(crate) fn pool(threads: usize) -> ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|index| format!("pbz2-{index}"))
        .build()
        .expect("the operating system starts the threads")
}
