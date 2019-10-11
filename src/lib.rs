//! Queue uses packed atomics to store information about writes, allowing
//! emulation of an infinite queue in a constant size allocation. This means
//! that a finite number of writes are supported. After around 2^63 - `size`
//! writes, information will overlap in the packed atomics, breaking
//! synchronization in the queue. `size` refers to the allocation size, which
//! is the requested queue size rounded up to a power of two. Note that his
//! happens before integer overflow.

use std::cell::Cell as ICell;
use std::sync::atomic::{AtomicUsize, Ordering::*};
use std::usize;
// https://github.com/rust-lang/rfcs/blob/master/text/1443-extended-compare-and-swap.md

/// Write epochs: 0 represents defualt data, 1 is the first valid write
#[derive(Debug, Default)]
pub struct Cell<T: Copy> {
    data: ICell<T>,
    epoch: AtomicUsize,
}

#[cfg(target_pointer_width = "16")]
const SENTINEL_MASK: usize = 1 << 15;

#[cfg(target_pointer_width = "32")]
const SENTINEL_MASK: usize = 1 << 31;

#[cfg(target_pointer_width = "64")]
const SENTINEL_MASK: usize = 1 << 63;

impl<T: Copy> Cell<T> {
    #[inline]
    pub fn write(&self, dat: T, new_epoch: usize, epoch_increment: usize) {
        // little CAS loop to ensure exclusive, complete, sequential writes
        // downside: newer writes can't "kick" off old writers
        // though, a sufficiently large queue will ensure this basically never happens as long
        // as each writer gets a chance to run

        let old_epoch = new_epoch - epoch_increment;
        loop {
            // Could possibly downgrade seqcst to acqrel
            match self.epoch.compare_exchange_weak(
                old_epoch,
                new_epoch | SENTINEL_MASK,
                SeqCst,
                Acquire,
            ) {
                Ok(_) => break,
                // if any race occurs, there's a chance for a deadlock here
                // ensure the epoch we are trying to advance from comes before us
                // if not, we will be stuck in a loop forever and have big problems
                Err(x) => debug_assert!(x & !SENTINEL_MASK <= old_epoch),
            }
        }
        self.data.set(dat);
        self.epoch.store(new_epoch, Release);
    }

    #[inline]
    pub fn read(&self) -> T {
        self.data.get()
    }

    #[inline]
    pub fn epoch(&self) -> &AtomicUsize {
        &self.epoch
    }
}

#[derive(Debug)]
pub struct Queue<T: Copy> {
    data: Box<[Cell<T>]>,
    write_ptr: AtomicUsize,
    idx_mask: usize,
}

impl<T: Default + Copy> Queue<T> {
    pub fn new(size: usize) -> Self {
        assert!(size > 0);
        let size = round_up_to_power_of_two(size);
        let mut data = Vec::with_capacity(size);
        // the vec! macro requires a Clone bound
        for _ in 0..size {
            data.push(Default::default());
        }
        Self {
            data: data.into_boxed_slice(),
            write_ptr: AtomicUsize::new(size), // write epoch 1, idx 0
            idx_mask: size - 1,
        }
    }

    #[inline]
    fn size(&self) -> usize {
        self.idx_mask + 1
    }

    #[inline]
    fn epoch(&self, idx: usize) -> usize {
        idx & (!self.idx_mask)
    }

    #[inline]
    fn modu(&self, idx: usize) -> usize {
        idx & self.idx_mask
    }

    #[inline]
    fn next_write_ptr(&self) -> usize {
        self.write_ptr.load(Acquire)
    }

    #[inline]
    pub fn push(&self, data: T) {
        // CAS loop until we get our turn to write
        let mut old = self.write_ptr.load(Relaxed);
        loop {
            let new = old + 1;
            match self
                .write_ptr
                .compare_exchange_weak(old, new, SeqCst, Relaxed) // Could maybe improve the success ordering
            {
                Ok(_) => break,
                Err(x) => old = x,
            }
        }
        // now we can write our data into old
        self.data[self.modu(old)].write(data, self.epoch(old), self.size());
    }

    /// Reads the last value that has a write initiated. Returns `None` if the write has not completed.
    /// Fails if nothing has been written to the queue.

    #[inline]
    pub fn try_read_latest(&self) -> Option<T> {
        let idx = self.write_ptr.load(Acquire) - 1;
        self.read(idx).ok()
    }

    /// Starts at the most recently initiated write, walking backwards until it finds a successful write.
    /// Will block indefinitely if nothing has been written to the queue yet.
    #[inline]
    pub fn read_latest(&self) -> T {
        let mut idx = self.write_ptr.load(Acquire) - 1;
        loop {
            match self.read(idx) {
                Ok(data) => {
                    return data;
                }
                Err(_epoch) => {
                    idx -= 1;
                }
            }
        }
    }

    /// Waits for the most recently initiated write to complete. Will not chase new writes after inovacation.
    #[inline]
    pub fn read_latest_blocking(&self) -> T {
        let idx = self.write_ptr.load(Acquire) - 1;
        loop {
            match self.read(idx) {
                Ok(data) => {
                    return data;
                }
                Err(_epoch) => (),
            }
        }
    }

    /// If the idx is still valid, returns Ok(T), else Err(epoch)
    #[inline]
    pub fn read(&self, idx: usize) -> Result<T, usize> {
        let cell = &self.data[self.modu(idx)];
        let epoch = cell.epoch.load(Acquire);
        if epoch != self.epoch(idx) {
            // if epochs don't match, it's over
            return Err(epoch);
        }
        let rr = cell.read();
        // ensure that no writes occurred while we were reading
        // a write would store a sentinel during the write if it
        // didn't complete, and a new epoch if it did.
        let epoch = cell.epoch.load(Acquire);
        if epoch != self.epoch(idx) {
            return Err(epoch);
        }
        Ok(rr)
    }
}

// The way Queue writes to Cell guarantees no data races
unsafe impl<T: Copy> Sync for Queue<T> {}

use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct QueueClient<T: Copy> {
    queue: Arc<Queue<T>>,
    to_read: usize,
}

impl<T: Copy + Default> QueueClient<T> {
    pub fn new_queue(size: usize) -> Self {
        let q = Queue::new(size);
        let to_read = q.size();
        Self {
            queue: Arc::new(q),
            to_read,
        }
    }

    /// Resets read stream to a valid message with a margin for writes before the next read.
    #[inline]
    pub fn catch_up(&mut self, margin: usize) {
        self.to_read = self.queue.next_write_ptr() - self.queue.size() + margin;
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.queue.size()
    }

    #[inline]
    pub fn push(&self, data: T) {
        self.queue.push(data)
    }

    /// May drop messages
    /// Returns None if all messages have been consumed
    pub fn next(&mut self) -> Option<T> {
        // "backoff" our catch up in case writes are really fast
        let mut margin = 1;
        let size = self.queue.size();
        while margin < size {
            match self.queue.read(self.to_read) {
                Ok(data) => {
                    self.to_read += 1;
                    return Some(data);
                }
                Err(epoch) => {
                    // first handle the case of a write
                    let write_in_progress = epoch & SENTINEL_MASK > 0;
                    let epoch = epoch & !SENTINEL_MASK;
                    if epoch <= self.queue.epoch(self.to_read) || write_in_progress {
                        // either, we are trying to read ahead, or trying to read data that is currently being written
                        return None;
                    } else {
                        // this means data_epoch > read_epoch, so the writers have overtaken us
                        self.catch_up(margin);
                    }
                }
            }
            margin *= 2;
        }
        // if all of our backoff doesnt work, something is seriously wrong
        None
    }

    /// May drop messages
    /// Blocks until there is a message to read
    #[inline]
    pub fn next_blocking(&mut self) -> T {
        loop {
            match self.next() {
                Some(data) => {
                    return data;
                }
                None => (),
            }
        }
    }

    #[inline]
    pub fn latest(&self) -> T {
        self.queue.read_latest()
    }
}


#[cfg(any(
    target_pointer_width = "64",
    target_pointer_width = "32",
    target_pointer_width = "16"
))]
fn round_up_to_power_of_two(mut u: usize) -> usize {
    u -= 1;
    u |= u >> 1;
    u |= u >> 2;
    u |= u >> 4;
    if cfg!(target_pointer_width = "16")
        || cfg!(target_pointer_width = "32")
        || cfg!(target_pointer_width = "64")
    {
        u |= u >> 8;
    }
    if cfg!(target_pointer_width = "32") || cfg!(target_pointer_width = "64") {
        u |= u >> 16;
    }
    if cfg!(target_pointer_width = "64") {
        u |= u >> 32;
    }
    u += 1;
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding() {
        assert_eq!(round_up_to_power_of_two(1), 1);
        assert_eq!(round_up_to_power_of_two(5), 8);
        assert_eq!(round_up_to_power_of_two(11), 16);
        assert_eq!(round_up_to_power_of_two(15), 16);
        assert_eq!(round_up_to_power_of_two(16), 16);
        assert_eq!(round_up_to_power_of_two(17), 32);
        assert_eq!(round_up_to_power_of_two(28), 32);
        assert_eq!(round_up_to_power_of_two(56), 64);
        assert_eq!(round_up_to_power_of_two(45), 64);
        assert_eq!(round_up_to_power_of_two(100), 128);
        assert_eq!(round_up_to_power_of_two(128), 128);
        assert_eq!(round_up_to_power_of_two(423), 512);
        assert_eq!(round_up_to_power_of_two(1000), 1024);
        assert_eq!(round_up_to_power_of_two(2000), 2048);
        assert_eq!(
            round_up_to_power_of_two(1_152_921_504_606_846_000),
            1_152_921_504_606_846_976
        );
        assert_eq!(
            round_up_to_power_of_two(9_223_372_036_854_000_000),
            9_223_372_036_854_775_808
        );
    }

    fn get_incrementor() -> impl Iterator<Item = u32> {
        std::iter::successors(Some(1), |n| Some(n + 1))
    }

    #[derive(Debug, Default, Clone)]
    struct Chomp(Option<u32>);

    impl Chomp {
        fn eat(&mut self, u: u32) {
            dbg!((u, self.0));
            match self.0 {
                None => (),
                Some(x) if x == u - 1 => (),
                Some(_) => panic!(),
            }
            self.0 = Some(u);
        }
    }

    fn write(q: &QueueClient<u32>, data_iter: &mut impl Iterator<Item = u32>, n: u32) {
        for _ in 0..n {
            q.push(data_iter.next().unwrap());
        }
    }

    fn read(q: &mut QueueClient<u32>, ch: &mut Chomp, n: u32) {
        for _ in 0..n {
            ch.eat(q.next_blocking());
        }
    }

    #[test]
    fn single_threaded_single_client() {
        let q = &mut QueueClient::new_queue(100);
        let di = &mut get_incrementor();
        let ch = &mut Chomp::default();
        write(q, di, 20);
        read(q, ch, 20);
        write(q, di, 30);
        read(q, ch, 20);
        write(q, di, 40);
        read(q, ch, 50);
        assert_eq!(q.next(), None);
        write(q, di, 1);
        read(q, ch, 1);
    }

    #[test]
    fn single_threaded_multi_client() {
        let q1 = &mut QueueClient::new_queue(100);
        let q2 = &mut Clone::clone(q1);
        let di = &mut get_incrementor();
        let c1 = &mut Chomp::default();
        let c2 = &mut Chomp::default();

        // indivudal
        write(q1, di, 20);
        read(q1, c1, 20);
        read(q2, c2, 20);

        // exchanging
        write(q1, di, 20);
        read(q2, c2, 20);
        write(q2, di, 20);
        read(q1, c1, 20);
        read(q2, c2, 20);
        read(q1, c1, 20);

        assert_eq!(q1.next(), None);
        write(q1, di, 1);
        read(q1, c1, 1);
        read(q2, c2, 1);

        assert_eq!(q2.next(), None);
        write(q2, di, 1);
        read(q2, c2, 1);
        read(q1, c1, 1);

        assert_eq!(q2.next(), None);
        assert_eq!(q1.next(), None);

        write(q1, di, 500);
        // 562 written so far
        // default read margin is 1
        // read 128 back, meaning
        assert_eq!(q2.next(), Some(62 + 500 - 127 + 1));
        assert_eq!(q1.latest(), 62 + 500);
    }
}
