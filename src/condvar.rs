use crate::mutex_v1::MutexGuard;
use atomic_wait::{wait, wake_all, wake_one};
use std::fmt::Debug;
use std::sync::atomic::{AtomicU32, Ordering};

struct CondVar {
    counter: AtomicU32,
    waiting: AtomicU32,
}

impl CondVar {
    pub fn new() -> CondVar {
        CondVar {
            counter: AtomicU32::new(0),
            waiting: AtomicU32::new(0),
        }
    }

    pub fn wait<'a, T>(&self, mutex_guard: MutexGuard<'a, T>) -> MutexGuard<'a, T>
    where
        T: Debug,
    {
        let state = self.counter.load(Ordering::Relaxed);
        self.waiting.fetch_add(1, Ordering::Relaxed);
        let mutex = mutex_guard.lock;
        mutex_guard.unlock();
        wait(&self.counter, state);
        self.waiting.fetch_sub(1, Ordering::Relaxed);
        mutex.lock().unwrap()
    }

    pub fn notify_one(&self) {
        self.counter.fetch_add(1, Ordering::Relaxed);
        wake_one(&self.counter);
    }

    pub fn notify_all(&self) {
        self.counter.fetch_add(1, Ordering::Relaxed);
        if self.waiting.load(Ordering::Relaxed) != 0 {
            wake_all(&self.counter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutex_v1::Mutex;
    use std::sync::{mpsc, Arc};
    use std::thread;

    #[test]
    fn test_basic_wait_notify_one() {
        let mutex = Arc::new(Mutex::new(0));
        let cvar = Arc::new(CondVar::new());
        let (tx, rx) = mpsc::channel();

        let mutex_clone = Arc::clone(&mutex);
        let cvar_clone = Arc::clone(&cvar);

        let handle = thread::spawn(move || {
            let mut guard = mutex_clone.lock().unwrap();
            tx.send(()).unwrap();
            while *guard < 1 {
                guard = cvar_clone.wait(guard);
            }

            assert_eq!(*guard, 1);
        });
        rx.recv().unwrap();

        {
            let mut guard = mutex.lock().unwrap();
            *guard = 1;
        }
        cvar.notify_one();
        handle.join().unwrap();
    }

    #[test]
    fn test_notify_all_multiple_waiters() {
        let mutex = Arc::new(Mutex::new(0));
        let cvar = Arc::new(CondVar::new());

        let thread_count = 8;
        let ready_count = Arc::new(AtomicU32::new(0));
        let mut handles = vec![];

        for _ in 0..thread_count {
            let mutex = Arc::clone(&mutex);
            let cvar = Arc::clone(&cvar);
            let ready_count = Arc::clone(&ready_count);

            handles.push(thread::spawn(move || {
                let mut guard = mutex.lock().unwrap();
                ready_count.fetch_add(1, Ordering::Relaxed);
                while *guard < 1 {
                    guard = cvar.wait(guard);
                }
                assert_eq!(*guard, 1);
            }));
        }

        while ready_count.load(Ordering::Relaxed) < thread_count {
            std::hint::spin_loop();
        }
        {
            let mut guard = mutex.lock().unwrap();
            *guard = 1;
            cvar.notify_all();
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_waiting_counter() {
        let mutex = Arc::new(Mutex::new(0));
        let cvar = Arc::new(CondVar::new());

        let (tx, rx) = mpsc::channel();

        let mutex_clone = Arc::clone(&mutex);
        let cvar_clone = Arc::clone(&cvar);

        assert_eq!(cvar.waiting.load(Ordering::Relaxed), 0);

        let handle = thread::spawn(move || {
            let guard = mutex_clone.lock().unwrap();
            tx.send(()).unwrap();
            cvar_clone.wait(guard);
        });

        rx.recv().unwrap();
        assert_eq!(cvar.waiting.load(Ordering::Relaxed), 1);

        cvar.notify_one();
        handle.join().unwrap();
        assert_eq!(cvar.waiting.load(Ordering::Relaxed), 0);
    }
}
