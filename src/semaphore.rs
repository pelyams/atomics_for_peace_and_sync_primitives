use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use atomic_wait::{wait, wake_one};

struct Semaphore {
    count: AtomicU32,
    num_waiting: AtomicU32,
}

impl Semaphore {
    const fn new(initial_count: u32) -> Self {
        Semaphore {
            count: AtomicU32::new(initial_count),
            num_waiting: AtomicU32::new(0),
        }
    }

    fn wait(&self) {
        self.num_waiting.fetch_add(1, Ordering::Relaxed);
        loop {
            let count = self.count.load(Ordering::Relaxed);
            if count == 0 {
                wait(&self.count, count);
            } else {
                if self.count.compare_exchange_weak(count, count - 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    self.num_waiting.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }

    fn try_wait(&self) -> bool {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 { return false; }
        self.count.compare_exchange_weak(count, count - 1, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    //just some busy looping
    fn try_wait_duration(&self, duration: Duration) -> bool {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            if self.try_wait() {
                return true;
            }
            std::hint::spin_loop();
        }
        false
    }

    fn signal(&self) {
        self.count.fetch_add(1, Ordering::Release);
        if self.num_waiting.load(Ordering::Relaxed) > 0 {
            wake_one(&self.count);
        }
    }
}

unsafe impl Send for Semaphore {}
unsafe impl Sync for Semaphore {}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;


    #[test]
    fn test_basics_signal_before_wait() {
        let semaphore = Semaphore::new(0);
        semaphore.signal();
        assert_eq!(semaphore.count.load(Ordering::Relaxed), 1);
        semaphore.wait();
        assert_eq!(semaphore.count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_basics_wait_before_signal() {
        let semaphore = Semaphore::new(1);
        semaphore.wait();
        assert_eq!(semaphore.count.load(Ordering::Relaxed), 0);
        semaphore.signal();
        assert_eq!(semaphore.count.load(Ordering::Relaxed), 1);

    }

    #[test]
    fn test_try_wait() {
        let semaphore = Semaphore::new(1);
        thread::scope(|s| {
            s.spawn(|| {
                semaphore.wait();
            });
        });
        assert!(!semaphore.try_wait());
    }

    #[test]
    fn test_multiple_threads() {
        let semaphore = Arc::new(Semaphore::new(2));
        let thread_count = 16;
        let mut handles = vec![];

        for _ in 0..thread_count {
            let sem_clone = Arc::clone(&semaphore);
            handles.push(std::thread::spawn(move || {
                sem_clone.wait();
                thread::sleep(Duration::from_millis(100));
                sem_clone.signal();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(semaphore.count.load(Ordering::Relaxed), 2);
        assert_eq!(semaphore.num_waiting.load(Ordering::Relaxed), 0);
    }


    // test attempts to find if there are lost signals due to race conditions
    #[test]
    fn test_racy_multiple_threads_signal_lost() {
        let semaphore = Arc::new(Semaphore::new(1));
        let thread_count = 16;
        let iterations = 50000;

        let barrier = Arc::new(Barrier::new(thread_count + 1));

        let threads: Vec<_> = (0..thread_count)
            .map(|_| {
                let sem_clone = Arc::clone(&semaphore);
                let barrier_clone = Arc::clone(&barrier);

                thread::spawn(move || {
                    barrier_clone.wait();
                    for _ in 0..iterations {
                        sem_clone.wait();
                        thread::sleep(Duration::from_nanos(1));
                        sem_clone.signal();
                    }
                })
            })
            .collect();

        barrier.wait();

        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(
            semaphore.count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "semaphore lost signals during concurrent operations"
        );
        assert_eq!(
            semaphore.num_waiting.load(std::sync::atomic::Ordering::Relaxed), 0
        )
    }


    #[test]
    fn test_multiple_waiters_access() {
        let max_concurrent_access_allowed = 4usize;
        let semaphore = Arc::new(Semaphore::new(max_concurrent_access_allowed as u32));
        let thread_count = 16;
        let iterations = 100;

        let concurrent_access_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_concurrent_access = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let barrier = Arc::new(Barrier::new(thread_count + 1));

        let threads: Vec<_> = (0..thread_count)
            .map(|_| {
                let sem_clone = Arc::clone(&semaphore);
                let barrier_clone = Arc::clone(&barrier);
                let concurrent_access_count_clone = Arc::clone(&concurrent_access_count);
                let max_concurrent_access_clone = Arc::clone(&max_concurrent_access);

                thread::spawn(move || {
                    barrier_clone.wait();

                    for _ in 0..iterations {
                        sem_clone.wait();

                        let current = concurrent_access_count_clone.fetch_add(1, Ordering::Relaxed) + 1;
                        max_concurrent_access_clone.fetch_max(current, Ordering::Relaxed);

                        thread::sleep(Duration::from_nanos(50));

                        concurrent_access_count_clone.fetch_sub(1, Ordering::Relaxed);
                        sem_clone.signal();
                    }
                })
            })
            .collect();

        barrier.wait();

        for thread in threads {
            thread.join().unwrap();
        }

        assert!(
            max_concurrent_access.load(Ordering::Relaxed) <= max_concurrent_access_allowed,
            "semaphore allowed more concurrent accesses than it was allowed"
        );
    }
}