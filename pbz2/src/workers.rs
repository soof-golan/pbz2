use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

struct Queue<T> {
    tasks: VecDeque<T>,
    closed: bool,
}

struct Shared<T> {
    queue: Mutex<Queue<T>>,
    ready: Condvar,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, Queue<T>> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn next(&self) -> Option<T> {
        let mut queue = self.lock();
        loop {
            if let Some(task) = queue.tasks.pop_front() {
                return Some(task);
            }
            if queue.closed {
                return None;
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

pub(crate) struct Workers<T> {
    shared: Arc<Shared<T>>,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + 'static> Workers<T> {
    pub(crate) fn new<F>(threads: usize, work: F) -> Self
    where
        F: Fn(T) + Clone + Send + 'static,
    {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                tasks: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        });
        let threads = (0..threads.max(1))
            .map(|index| {
                let shared = Arc::clone(&shared);
                let work = work.clone();
                thread::Builder::new()
                    .name(format!("pbz2-{index}"))
                    .spawn(move || {
                        while let Some(task) = shared.next() {
                            work(task);
                        }
                    })
                    .expect("the operating system starts the threads")
            })
            .collect();
        Self { shared, threads }
    }

    pub(crate) fn send(&self, task: T) {
        self.shared.lock().tasks.push_back(task);
        self.shared.ready.notify_one();
    }
}

impl<T> Drop for Workers<T> {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
