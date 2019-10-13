# LFQ

[![Crates.io](https://img.shields.io/crates/v/lfq.svg)](https://crates.io/crates/bus)
[![Documentation](https://docs.rs/lfq/badge.svg)](https://docs.rs/lfq/)

A lock-free multi-producer/multi-consumer broadcast queue backed by a ring
buffer.

Broadcast means that every reader will read every write.
Read the design section to see if the tradeoffs are right for your
application.

## Design

For speed reasons, the queue will not ever allocate after initial creation.
Infinite size is emulated via a ring-buffer in a constant size allocation.
To avoid blocking, writers are free to overwrite data that some or all
readers have yet to consume. This means that readers are not guaranteed
to see all writes. As such, this queue is unfit for anything resembling a
task queue. Use `crossbeam-channel` or `bus` for that.

Multi-consumer and broadcast means that only `Copy` types are supported.

Since streaming readers need to know if writers have overwritten their
place in the buffer, each unit of data and index into the queue has an
associated write "epoch". This data, along with a write-in-progress tag,
is stored into an `AtomicUsize`. For this reason, allocations sizes are
rounded up to a power of two. After around `2^(ptr width) - size`
writes, information will overlap in the packed atomics, breaking the queue
in unpredictable ways. `size` refers to the allocation size, not the
user-requested size. Note that this happens before integer overflow.

Writes are four step process. First, writers race for the next slot.
The winning writer then initiates the write to the buffer slot with
an atomic store, does the actual write, and then confirms it with another
atomic store. This ensures that readers will never see half-written data,
even if the data is larger than the size of atomic operations on the
platform.

Readers will check whether the cell they are reading from has an epoch that
matches their index. They will also reject any cells currently undergoing a
write (steps 2-4 above). This means that streaming reads are not guaranteed
to get every message. Moreover, reads of the latest write have to deal with
possibly incomplete writes. Various choices are implemented as public
methods.

The element type must implement `Default`, as several runtime checks are
eliminated by filling the internal buffer with default data. However, this
temporary data is never read and exists only to avoid `unsafe`.

The only unsafe code is a `Sync` impl on the internal `Queue` type.

## Example

```rust
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use lfq::QueueClient;

let w = QueueClient::new_queue(100);
assert_eq!(w.size(), 128);

let mut r = w.clone();
let messages = w.size() * 20;

let finished = Arc::new(AtomicBool::new(false));
let stop = finished.clone();
let thread = thread::spawn(move || {
    let mut last = 0;
    while !stop.load(Ordering::Relaxed) {
        let result = r.latest();
        assert!(result >= last);
        last = result;
    }
});

for data in 0..messages {
    w.push(data);
}
finished.store(true, Ordering::Relaxed);
thread.join().unwrap();
```
