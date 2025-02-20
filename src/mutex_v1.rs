use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::Thread;
use thread_id;

use crate::utils::mutex_common::{LockError, PoisonedLock};

pub type LockResult<Guard> = Result<Guard, LockError<Guard>>;

#[derive(Debug)]
pub struct Mutex<T> {
    data: UnsafeCell<T>,
    queue_is_accessed: AtomicBool,
    state: AtomicUsize,
    queue: UnsafeCell<std::collections::VecDeque<Thread>>,
    poisoned: UnsafeCell<bool>,
}

impl<T> Mutex<T> {
    pub fn new(data: T) -> Mutex<T> {
        Mutex {
            data: data.into(),
            queue_is_accessed: AtomicBool::new(false),
            state: AtomicUsize::new(0),
            queue: std::collections::VecDeque::new().into(),
            poisoned: false.into(),
        }
    }
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let current_thread = thread_id::get();
        let state = self.state.load(Ordering::Relaxed);
        if state == current_thread {
            return Err(LockError::WouldBlock);
        }
        'outer: loop {
            let mut spin_counter = 0;
            while spin_counter < 100 {
                let state = self.state.load(Ordering::Relaxed);
                match state {
                    0 => {
                        if self
                            .state
                            .compare_exchange_weak(
                                0,
                                current_thread,
                                Ordering::Acquire,
                                Ordering::Relaxed,
                            )
                            .is_ok()
                        {
                            break 'outer;
                        };
                    }
                    _ => {
                        spin_counter += 1;
                    }
                }
                std::hint::spin_loop();
            }
            self.enqueue_thread();
        }
        if self.is_poisoned() {
            return Err(LockError::Poisoned(PoisonedLock {
                guard: MutexGuard { lock: self },
            }));
        }

        Ok(MutexGuard { lock: self })
    }
    pub fn try_lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let current_thread = thread_id::get();
        let state = self.state.load(Ordering::Relaxed);
        if state == current_thread {
            return Err(LockError::WouldBlock);
        }
        if self
            .state
            .compare_exchange(0, current_thread, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            if self.is_poisoned() {
                return Err(LockError::Poisoned(PoisonedLock {
                    guard: MutexGuard { lock: self },
                }));
            }
            return Ok(MutexGuard { lock: self });
        }
        Err(LockError::WouldBlock)
    }
    pub fn is_poisoned(&self) -> bool {
        unsafe { *self.poisoned.get() }
    }
    pub fn clear_poison(&self) {
        unsafe { *self.poisoned.get() = false };
    }

    //latter two should ideally be packed into Result returning some err(T)
    //if mutex is poisoned
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    fn enqueue_thread(&self) {
        loop {
            let queue_accessed = self.queue_is_accessed.load(Ordering::Relaxed);
            if !queue_accessed {
                if self
                    .queue_is_accessed
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let current_thread = std::thread::current();
                    unsafe { (*self.queue.get()).push_back(current_thread) };
                    self.queue_is_accessed.store(false, Ordering::Release);
                    std::thread::park();

                    return;
                }
            }
            std::hint::spin_loop();
        }
    }

    fn dequeue_thread(&self) {
        loop {
            let queue_accessed = self.queue_is_accessed.load(Ordering::Relaxed);
            if !queue_accessed {
                if self
                    .queue_is_accessed
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let queue = self.queue.get();
                    let has_more_waiters = unsafe { !(*queue).is_empty() };
                    if has_more_waiters {
                        let thread_to_unpark = unsafe { (*queue).pop_front() };
                        self.queue_is_accessed.store(false, Ordering::Release);
                        if let Some(t) = thread_to_unpark {
                            t.unpark();
                        }
                    } else {
                        self.queue_is_accessed.store(false, Ordering::Release);
                    }
                    return;
                }
            }
            std::hint::spin_loop();
        }
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

#[derive(Debug)]
pub struct MutexGuard<'a, T> {
    pub(crate) lock: &'a Mutex<T>,
}

impl<T> MutexGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe { *self.lock.poisoned.get() = true };
        }
        self.lock.state.store(0, Ordering::Release);
        self.lock.dequeue_thread();
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> !Send for MutexGuard<'_, T> {}

#[cfg(test)]
mod tests {
    use crate::mutex_v1::{LockError, Mutex};

    #[test]
    fn test_basic_locking() {
        let mutex = Mutex::new(4u8);
        let locked = mutex.lock().unwrap();
        assert_eq!(*locked, 4);
        drop(locked);
        let mut locked = mutex.lock().unwrap();
        *locked *= 20;
        assert_eq!(*locked, 80);
    }

    #[test]
    fn test_try_lock() {
        let mutex = Mutex::new(4u8);
        let locked = mutex.lock();
        let try_locked = mutex.try_lock();
        assert!(locked.is_ok());
        assert!(try_locked.is_err());
    }

    #[test]
    fn test_concurrency() {
        let mutex = std::sync::Arc::new(Mutex::new(0u8));
        {
            let mutex = mutex.clone();
            std::thread::scope(|s| {
                (0..100).for_each(|_| {
                    let mutex = mutex.clone();
                    let _ = s.spawn(move || {
                        let mut lock = mutex.lock().unwrap();
                        *lock += 1;
                    });
                });
            });
        }
        let mut lock = mutex.lock().unwrap();
        *lock += 11;
        assert_eq!(*lock, 111);
    }

    #[test]
    fn test_reentrancy() {
        let mutex = Mutex::new("shared data");
        let first_lock = mutex.lock();
        let second_lock = mutex.lock();
        assert!(first_lock.is_ok());
        assert!(second_lock.is_err());
        assert_eq!(second_lock.unwrap_err(), LockError::WouldBlock);
    }

    #[test]
    #[should_panic]
    fn test_unlocking_after_panic() {
        let m = Mutex::new(0u8);
        {
            _ = std::thread::scope(|s| {
                let mut ml = m.lock().unwrap();
                *ml += 100;
                panic!("accidentally panicked");
                //here comes senseless line:
                *ml += 100;
            });
        }
        let lock_attempt_one = m.lock();
        assert!(lock_attempt_one.is_err());
        assert!(m.is_poisoned());
        m.clear_poison();
        drop(lock_attempt_one);
        let lock_attempt_two = m.lock();
        assert!(lock_attempt_two.is_ok());
        assert_eq!(*lock_attempt_two.unwrap(), 100);
    }

    #[test]
    fn test_contended() {
        let m = Mutex::new(1);
        std::hint::black_box(&m);
        let start = std::time::Instant::now();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..2500000 {
                        *m.lock().unwrap() += 1;
                    }
                });
            }
        });
        let duration = start.elapsed();
        println!("locked {} times in {:?}", *m.lock().unwrap(), duration);
    }
}
