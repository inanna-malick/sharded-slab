use crate::Slab;
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod idx {
    use crate::{
        cfg,
        page::{self, slot},
        Pack, Tid,
    };
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tid_roundtrips(tid in 0usize..Tid::<cfg::DefaultConfig>::BITS) {
            let tid = Tid::<cfg::DefaultConfig>::from_usize(tid);
            let packed = tid.pack(0);
            assert_eq!(tid, Tid::from_packed(packed));
        }

        #[test]
        fn idx_roundtrips(
            tid in 0usize..Tid::<cfg::DefaultConfig>::BITS,
            gen in 0usize..slot::Generation::<cfg::DefaultConfig>::BITS,
            addr in 0usize..page::Addr::<cfg::DefaultConfig>::BITS,
        ) {
            let tid = Tid::<cfg::DefaultConfig>::from_usize(tid);
            let gen = slot::Generation::<cfg::DefaultConfig>::from_usize(gen);
            let addr = page::Addr::<cfg::DefaultConfig>::from_usize(addr);
            let packed = tid.pack(gen.pack(addr.pack(0)));
            assert_eq!(addr, page::Addr::from_packed(packed));
            assert_eq!(gen, slot::Generation::from_packed(packed));
            assert_eq!(tid, Tid::from_packed(packed));
        }
    }
}

pub(crate) struct TinyConfig;

impl crate::Config for TinyConfig {
    const INITIAL_PAGE_SIZE: usize = 4;
}

use self::util::*;
pub(super) mod util {
    use super::*;

    pub(crate) fn run_model(name: &'static str, f: impl Fn() + Sync + Send + 'static) {
        run_builder(name, loom::model::Builder::new(), f)
    }

    pub(crate) fn run_builder(
        name: &'static str,
        builder: loom::model::Builder,
        f: impl Fn() + Sync + Send + 'static,
    ) {
        let iters = AtomicUsize::new(1);
        builder.check(move || {
            test_println!(
                "\n------------ running test {}; iteration {} ------------\n",
                name,
                iters.fetch_add(1, Ordering::SeqCst)
            );
            f()
        });
    }
}

#[test]
fn take_local() {
    run_model("take_local", || {
        let slab = Arc::new(Slab::new());

        let s = slab.clone();
        let t1 = thread::spawn(move || {
            let idx = s.insert(1).expect("insert");
            assert_eq!(s.get(idx).unwrap(), 1);
            assert_eq!(s.take(idx), Some(1));
            assert!(s.get(idx).is_none());
            let idx = s.insert(2).expect("insert");
            assert_eq!(s.get(idx).unwrap(), 2);
            assert_eq!(s.take(idx), Some(2));
            assert!(s.get(idx).is_none());
        });

        let s = slab.clone();
        let t2 = thread::spawn(move || {
            let idx = s.insert(3).expect("insert");
            assert_eq!(s.get(idx).unwrap(), 3);
            assert_eq!(s.take(idx), Some(3));
            assert!(s.get(idx).is_none());
            let idx = s.insert(4).expect("insert");
            assert_eq!(s.get(idx).unwrap(), 4);
            assert_eq!(s.take(idx), Some(4));
            assert!(s.get(idx).is_none());
        });

        let s = slab;
        let idx1 = s.insert(5).expect("insert");
        assert_eq!(s.get(idx1).unwrap(), 5);
        let idx2 = s.insert(6).expect("insert");
        assert_eq!(s.get(idx2).unwrap(), 6);
        assert_eq!(s.take(idx1), Some(5));
        assert!(s.get(idx1).is_none());
        assert_eq!(s.get(idx2).unwrap(), 6);
        assert_eq!(s.take(idx2), Some(6));
        assert!(s.get(idx2).is_none());

        t1.join().expect("thread 1 should not panic");
        t2.join().expect("thread 2 should not panic");
    });
}

#[test]
fn get_mut_local() {
    run_model("get_mut_local", || {
        let slab = Arc::new(Slab::new());
        let pair = Arc::new((Mutex::new(None), Condvar::new()));

        let idx = slab.insert(1).expect("insert");
        assert_eq!(slab.get_mut(idx).unwrap(), 1);

        let s = slab.clone();
        let pair1 = pair.clone();
        let t1 = thread::spawn(move || {
            let (lock, cvar) = &*pair1;

            let guard = s.get(idx);

            {
                // Let the other thread try to get mutable access after we have immutable access
                let mut next = lock.lock().unwrap();
                *next = Some(());
                cvar.notify_one();
            }

            let mut next = lock.lock().unwrap();
            // wait until other thread has mutable access before dropping guard
            while next.is_some() {
                next = cvar.wait(next).unwrap();
            }

            drop(guard);
        });

        let (lock, cvar) = &*pair;

        {
            let mut next = lock.lock().unwrap();
            // wait until the other thread has gained immutable access
            while next.is_none() {
                next = cvar.wait(next).unwrap();
            }
        }

        let guard = slab.get_mut(idx);
        assert!(guard.is_err());

        let mut next = lock.lock().unwrap();
        *next = None;
        // notify once we have tried and errored out
        cvar.notify_one();

        // Make sure to drop mutex guard so the other thread can make progress
        drop(next);
        t1.join().expect("thread 1 should not panic");
    })
}

#[test]
fn take_remote() {
    run_model("take_remote", || {
        let slab = Arc::new(Slab::new());

        let idx1 = slab.insert(1).expect("insert");
        assert_eq!(slab.get(idx1).unwrap(), 1);
        let idx2 = slab.insert(2).expect("insert");
        assert_eq!(slab.get(idx2).unwrap(), 2);

        let idx3 = slab.insert(3).expect("insert");
        assert_eq!(slab.get(idx3).unwrap(), 3);

        let s = slab.clone();
        let t1 = thread::spawn(move || {
            assert_eq!(s.get(idx2).unwrap(), 2);
            assert_eq!(s.take(idx2), Some(2));
        });

        let s = slab.clone();
        let t2 = thread::spawn(move || {
            assert_eq!(s.get(idx3).unwrap(), 3);
            assert_eq!(s.take(idx3), Some(3));
        });

        t1.join().expect("thread 1 should not panic");
        t2.join().expect("thread 2 should not panic");

        assert_eq!(slab.get(idx1).unwrap(), 1);
        assert!(slab.get(idx2).is_none());
        assert!(slab.get(idx3).is_none());
    });
}

#[test]
fn racy_take() {
    run_model("racy_take", || {
        let slab = Arc::new(Slab::new());

        let idx = slab.insert(1).expect("insert");
        assert_eq!(slab.get(idx).unwrap(), 1);

        let s1 = slab.clone();
        let s2 = slab.clone();

        let t1 = thread::spawn(move || s1.take(idx));
        let t2 = thread::spawn(move || s2.take(idx));

        let r1 = t1.join().expect("thread 1 should not panic");
        let r2 = t2.join().expect("thread 2 should not panic");

        assert!(
            r1.is_none() || r2.is_none(),
            "both threads should not have removed the value"
        );
        assert_eq!(
            r1.or(r2),
            Some(1),
            "one thread should have removed the value"
        );
        assert!(slab.get(idx).is_none());
    });
}

#[test]
fn racy_take_local() {
    run_model("racy_take_local", || {
        let slab = Arc::new(Slab::new());

        let idx = slab.insert(1).expect("insert");
        assert_eq!(slab.get(idx).unwrap(), 1);

        let s = slab.clone();
        let t2 = thread::spawn(move || s.take(idx));
        let r1 = slab.take(idx);
        let r2 = t2.join().expect("thread 2 should not panic");

        assert!(
            r1.is_none() || r2.is_none(),
            "both threads should not have removed the value"
        );
        assert!(
            r1.or(r2).is_some(),
            "one thread should have removed the value"
        );
        assert!(slab.get(idx).is_none());
    });
}

#[test]
fn concurrent_insert_take() {
    run_model("concurrent_insert_remove", || {
        let slab = Arc::new(Slab::new());
        let pair = Arc::new((Mutex::new(None), Condvar::new()));

        let slab2 = slab.clone();
        let pair2 = pair.clone();
        let remover = thread::spawn(move || {
            let (lock, cvar) = &*pair2;
            for i in 0..2 {
                test_println!("--- remover i={} ---", i);
                let mut next = lock.lock().unwrap();
                while next.is_none() {
                    next = cvar.wait(next).unwrap();
                }
                let key = next.take().unwrap();
                assert_eq!(slab2.take(key), Some(i));
                cvar.notify_one();
            }
        });

        let (lock, cvar) = &*pair;
        for i in 0..2 {
            test_println!("--- inserter i={} ---", i);
            let key = slab.insert(i).expect("insert");

            let mut next = lock.lock().unwrap();
            *next = Some(key);
            cvar.notify_one();

            // Wait for the item to be removed.
            while next.is_some() {
                next = cvar.wait(next).unwrap();
            }

            assert!(slab.get(key).is_none());
        }

        remover.join().unwrap();
    })
}

#[test]
fn take_remote_and_reuse() {
    run_model("take_remote_and_reuse", || {
        let slab = Arc::new(Slab::new_with_config::<TinyConfig>());

        let idx1 = slab.insert(1).expect("insert");
        let idx2 = slab.insert(2).expect("insert");
        let idx3 = slab.insert(3).expect("insert");
        let idx4 = slab.insert(4).expect("insert");

        assert_eq!(slab.get(idx1).unwrap(), 1, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx2).unwrap(), 2, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx3).unwrap(), 3, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx4).unwrap(), 4, "slab: {:#?}", slab);

        let s = slab.clone();
        let t1 = thread::spawn(move || {
            assert_eq!(s.take(idx1), Some(1), "slab: {:#?}", s);
        });

        let idx1 = slab.insert(5).expect("insert");
        t1.join().expect("thread 1 should not panic");

        assert_eq!(slab.get(idx1).unwrap(), 5, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx2).unwrap(), 2, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx3).unwrap(), 3, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx4).unwrap(), 4, "slab: {:#?}", slab);
    });
}

fn store_when_free<C: crate::Config>(slab: &Arc<Slab<usize, C>>, t: usize) -> usize {
    loop {
        test_println!("try store {:?}", t);
        if let Some(key) = slab.insert(t) {
            test_println!("inserted at {:#x}", key);
            return key;
        }
        test_println!("retrying; slab is full...");
        thread::yield_now();
    }
}

struct TinierConfig;

impl crate::Config for TinierConfig {
    const INITIAL_PAGE_SIZE: usize = 2;
    const MAX_PAGES: usize = 1;
}

#[test]
fn concurrent_remove_remote_and_reuse() {
    let mut model = loom::model::Builder::new();
    model.max_branches = 100000;
    run_builder("concurrent_remove_remote_and_reuse", model, || {
        let slab = Arc::new(Slab::new_with_config::<TinierConfig>());

        let idx1 = slab.insert(1).unwrap();
        let idx2 = slab.insert(2).unwrap();

        assert_eq!(slab.get(idx1).unwrap(), 1, "slab: {:#?}", slab);
        assert_eq!(slab.get(idx2).unwrap(), 2, "slab: {:#?}", slab);

        let s = slab.clone();
        let s2 = slab.clone();

        let t1 = thread::spawn(move || {
            s.take(idx1).expect("must remove");
        });

        let t2 = thread::spawn(move || {
            s2.take(idx2).expect("must remove");
        });

        let idx3 = store_when_free(&slab, 3);
        t1.join().expect("thread 1 should not panic");
        t2.join().expect("thread 1 should not panic");

        assert!(slab.get(idx1).is_none(), "slab: {:#?}", slab);
        assert!(slab.get(idx2).is_none(), "slab: {:#?}", slab);
        assert_eq!(slab.get(idx3).unwrap(), 3, "slab: {:#?}", slab);
    });
}

struct SetDropped {
    value: usize,
    dropped: std::sync::Arc<AtomicBool>,
}

struct AssertDropped {
    dropped: std::sync::Arc<AtomicBool>,
}

impl AssertDropped {
    fn new(value: usize) -> (Self, SetDropped) {
        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        let val = SetDropped {
            value,
            dropped: dropped.clone(),
        };
        (Self { dropped }, val)
    }

    fn assert_dropped(&self) {
        assert!(
            self.dropped.load(Ordering::SeqCst),
            "value should have been dropped!"
        );
    }
}

impl Drop for SetDropped {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn remove_local() {
    run_model("remove_local", || {
        let slab = Arc::new(Slab::new_with_config::<TinyConfig>());
        let slab2 = slab.clone();

        let (dropped, item) = AssertDropped::new(1);
        let idx = slab.insert(item).expect("insert");

        let guard = slab.get(idx).unwrap();

        assert!(slab.remove(idx));

        let t1 = thread::spawn(move || {
            let g = slab2.get(idx);
            drop(g);
        });

        assert!(slab.get(idx).is_none());

        t1.join().expect("thread 1 should not panic");

        drop(guard);
        assert!(slab.get(idx).is_none());
        dropped.assert_dropped();
    })
}

#[test]
fn remove_remote() {
    run_model("remove_remote", || {
        let slab = Arc::new(Slab::new_with_config::<TinyConfig>());
        let slab2 = slab.clone();

        let (dropped, item) = AssertDropped::new(1);
        let idx = slab.insert(item).expect("insert");

        assert!(slab.remove(idx));
        let t1 = thread::spawn(move || {
            let g = slab2.get(idx);
            drop(g);
        });

        t1.join().expect("thread 1 should not panic");

        assert!(slab.get(idx).is_none());
        dropped.assert_dropped();
    });
}

#[test]
fn remove_remote_during_insert() {
    run_model("remove_remote_during_insert", || {
        let slab = Arc::new(Slab::new_with_config::<TinyConfig>());
        let slab2 = slab.clone();

        let (dropped, item) = AssertDropped::new(1);
        let idx = slab.insert(item).expect("insert");

        let t1 = thread::spawn(move || {
            let g = slab2.get(idx);
            assert_ne!(g.as_ref().map(|v| v.value), Some(2));
            drop(g);
        });

        let (_, item) = AssertDropped::new(2);
        assert!(slab.remove(idx));
        let idx2 = slab.insert(item).expect("insert");

        t1.join().expect("thread 1 should not panic");

        assert!(slab.get(idx).is_none());
        assert!(slab.get(idx2).is_some());
        dropped.assert_dropped();
    });
}

#[test]
fn unique_iter() {
    run_model("unique_iter", || {
        let mut slab = std::sync::Arc::new(Slab::new());

        let s = slab.clone();
        let t1 = thread::spawn(move || {
            s.insert(1).expect("insert");
            s.insert(2).expect("insert");
        });

        let s = slab.clone();
        let t2 = thread::spawn(move || {
            s.insert(3).expect("insert");
            s.insert(4).expect("insert");
        });

        t1.join().expect("thread 1 should not panic");
        t2.join().expect("thread 2 should not panic");

        let slab = std::sync::Arc::get_mut(&mut slab).expect("other arcs should be dropped");
        let items: Vec<_> = slab.unique_iter().map(|&i| i).collect();
        assert!(items.contains(&1), "items: {:?}", items);
        assert!(items.contains(&2), "items: {:?}", items);
        assert!(items.contains(&3), "items: {:?}", items);
        assert!(items.contains(&4), "items: {:?}", items);
    });
}

#[test]
fn custom_page_sz() {
    let mut model = loom::model::Builder::new();
    model.max_branches = 100000;
    model.check(|| {
        let slab = Slab::<usize>::new_with_config::<TinyConfig>();

        for i in 0..1024usize {
            test_println!("{}", i);
            let k = slab.insert(i).expect("insert");
            let v = slab.get(k).expect("get");
            assert_eq!(v, i, "slab: {:#?}", slab);
        }
    });
}

#[test]
fn max_refs() {
    struct LargeGenConfig;

    // Configure the slab with a very large number of bits for the generation
    // counter. That way, there will be very few bits for the ref count left
    // over, and this test won't have to malloc millions of references.
    impl crate::cfg::Config for LargeGenConfig {
        const INITIAL_PAGE_SIZE: usize = 2;
        const MAX_THREADS: usize = 32;
        const MAX_PAGES: usize = 2;
    }

    let mut model = loom::model::Builder::new();
    model.max_branches = 100000;
    model.check(|| {
        let slab = Slab::new_with_config::<LargeGenConfig>();
        let key = slab.insert("hello world").unwrap();
        let max = crate::page::slot::RefCount::<LargeGenConfig>::MAX;

        // Create the maximum number of concurrent references to the entry.
        let mut refs = (0..max)
            .map(|_| slab.get(key).unwrap())
            // Store the refs in a vec so they don't get dropped immediately.
            .collect::<Vec<_>>();

        assert!(slab.get(key).is_none());

        // After dropping a ref, we should now be able to access the slot again.
        drop(refs.pop());
        let ref1 = slab.get(key);
        assert!(ref1.is_some());

        // Ref1 should max out the number of references again.
        assert!(slab.get(key).is_none());
    })
}

mod free_list_reuse {
    use super::*;
    struct TinyConfig;

    impl crate::cfg::Config for TinyConfig {
        const INITIAL_PAGE_SIZE: usize = 2;
    }

    #[test]
    fn local_remove() {
        run_model("free_list_reuse::local_remove", || {
            let slab = Slab::new_with_config::<TinyConfig>();

            let t1 = slab.insert("hello").expect("insert");
            let t2 = slab.insert("world").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t1).1,
                0,
                "1st slot should be on 0th page"
            );
            assert_eq!(
                crate::page::indices::<TinyConfig>(t2).1,
                0,
                "2nd slot should be on 0th page"
            );
            let t3 = slab.insert("earth").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t3).1,
                1,
                "3rd slot should be on 1st page"
            );

            slab.remove(t2);
            let t4 = slab.insert("universe").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t4).1,
                0,
                "2nd slot should be reused (0th page)"
            );

            slab.remove(t1);
            let _ = slab.insert("goodbye").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t4).1,
                0,
                "1st slot should be reused (0th page)"
            );
        });
    }

    #[test]
    fn local_take() {
        run_model("free_list_reuse::local_take", || {
            let slab = Slab::new_with_config::<TinyConfig>();

            let t1 = slab.insert("hello").expect("insert");
            let t2 = slab.insert("world").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t1).1,
                0,
                "1st slot should be on 0th page"
            );
            assert_eq!(
                crate::page::indices::<TinyConfig>(t2).1,
                0,
                "2nd slot should be on 0th page"
            );
            let t3 = slab.insert("earth").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t3).1,
                1,
                "3rd slot should be on 1st page"
            );

            assert_eq!(slab.take(t2), Some("world"));
            let t4 = slab.insert("universe").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t4).1,
                0,
                "2nd slot should be reused (0th page)"
            );

            assert_eq!(slab.take(t1), Some("hello"));
            let _ = slab.insert("goodbye").expect("insert");
            assert_eq!(
                crate::page::indices::<TinyConfig>(t4).1,
                0,
                "1st slot should be reused (0th page)"
            );
        });
    }
}

#[test]
fn get_mut_basic() {
    run_model("get_mut_basic", || {
        let slab = Arc::new(Slab::new());

        let idx1 = slab.insert(String::from("hello world")).expect("create");
        let slab2 = slab.clone();
        let t1 = thread::spawn(move || {
            let v = slab2.get(idx1);
            if let Some(v) = v {
                assert_eq!(v, String::from("hello world"))
            }
        });

        let slab2 = slab.clone();
        let t2 = thread::spawn(move || {
            let v = slab2.get(idx1);
            if let Some(v) = v {
                assert_eq!(v, String::from("hello world"))
            }
        });
        let _mut = slab.get_mut(idx1);
        t1.join().unwrap();
        t2.join().unwrap();
    })
}

#[test]
fn get_mut_contended() {
    run_model("get_mut_contended", || {
        let slab = Arc::new(Slab::new());

        let idx1 = slab.insert(String::from("hello world")).expect("create");
        let slab2 = slab.clone();
        let t1 = thread::spawn(move || {
            let v = slab2.get_mut(idx1);
            if let Ok(v) = v {
                assert_eq!(v, String::from("hello world"))
            }
        });

        let slab2 = slab.clone();
        let t2 = thread::spawn(move || {
            let v = slab2.get_mut(idx1);
            if let Ok(v) = v {
                assert_eq!(v, String::from("hello world"))
            }
        });
        let _mut = slab.get_mut(idx1);
        t1.join().unwrap();
        t2.join().unwrap();
    })
}
