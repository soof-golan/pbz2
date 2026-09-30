use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::thread;

thread_local! {
    static SCRATCH: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static BLOCK: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn with_block<R>(bytes: usize, work: impl FnOnce(&mut [u8]) -> R) -> R {
    BLOCK.with_borrow_mut(|block| {
        if block.len() < bytes {
            *block = vec![0; bytes];
        }
        work(&mut block[..bytes])
    })
}

pub(crate) fn with_scratch<R>(words: usize, work: impl FnOnce(&mut [u32]) -> R) -> R {
    SCRATCH.with_borrow_mut(|scratch| {
        if scratch.len() < words {
            *scratch = vec![0; words];
        }
        work(&mut scratch[..words])
    })
}

pub(crate) fn every_core() -> usize {
    thread::available_parallelism().map_or(1, NonZeroUsize::get)
}
