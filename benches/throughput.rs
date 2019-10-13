use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::black_box;
use criterion::Throughput;


use std::convert::TryInto;

use std::thread;
use lfq::*;

use std::time::Instant;

use std::sync::{Arc, atomic::{AtomicBool, Ordering::SeqCst}};

#[derive(Default)]
struct DummyThreadManager {
    handles: Vec<(thread::JoinHandle<()>, Arc<AtomicBool>)>,
}

impl DummyThreadManager {
    fn add_thread(&mut self, f: impl FnOnce(Arc<AtomicBool>) + Send + 'static) {
        let a = Arc::new(AtomicBool::new(true));
        let b = a.clone();
        let h = thread::spawn(move || {
            f(b)
        });
        self.handles.push((h,a));
    }

    fn join_all(mut self) {
        (&mut self).handles.iter_mut().for_each(|(_h, b)| b.store(false, SeqCst));
        self.handles.into_iter().for_each(|(h, _b)| h.join().unwrap());
    }
}

#[derive(Default, Copy, Debug, Clone)]
struct DataDummy(u64, f64, f64, f64);
const DATA_DEFAULT: DataDummy = DataDummy(1233123, 30912831.132213, -12931.123, 123.98);

fn single_producer(c: &mut Criterion) {

    let mut g = c.benchmark_group("My Group");

    const QSIZE: usize = 128;
    const ITEMS: usize = QSIZE * 10;

    g.throughput(Throughput::Elements(ITEMS as u64));

    for consumers in 1..=15 {
        g.bench_with_input(BenchmarkId::new("Multiply", consumers), &consumers,
            |b, consumers| b.iter_custom(|iters| {
                eprintln!("iters {}", iters);
                let q = QueueClient::<DataDummy>::new_queue(QSIZE);
                let mut tm = DummyThreadManager::default();
                for _ in 0..*consumers {
                    let mut qp = q.clone();
                    tm.add_thread(move |b| {
                        while b.load(SeqCst) {
                            for _ in 0..100 {
                                black_box(qp.next());
                            }
                        }
                    })
                }
                let start = Instant::now();
                for _i in 0..iters {
                    for _ in 0..ITEMS {
                        q.push(DATA_DEFAULT);
                    }

                }
                let el = start.elapsed();
                tm.join_all();
                el
            })
        );
    }

    g.finish()
}

criterion_group!{
    name = benches;
    config = Criterion::default();
    targets = single_producer
}
criterion_main!(benches);
