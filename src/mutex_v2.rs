use atomic_wait::{wait, wake_one};
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::utils::mutex_common::{LockError, PoisonedLock};

pub type LockResult<Guard> = Result<Guard, LockError<Guard>>;

thread_local! {
    static ACQUIRED_BY_THREAD: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const LOCKED_HAS_WAITERS: u32 = 2;

#[derive(Debug)]
pub struct Mutex<T> {
    mutex_id: u64,
    data: UnsafeCell<T>,
    state: AtomicU32,
    poisoned: UnsafeCell<bool>,
}

impl<T> Mutex<T> {
    pub fn new(data: T) -> Mutex<T> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        Mutex {
            mutex_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            data: data.into(),
            state: AtomicU32::new(0),
            poisoned: false.into(),
        }
    }
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        _ = self.acquired_check()?;
        if self
            .state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            let mut counter = 0;
            while counter < 100 && self.state.load(Ordering::Relaxed) != UNLOCKED {
                counter += 1;
                std::hint::spin_loop();
            }
            if self
                .state
                .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                while self
                    .state
                    .compare_exchange(
                        UNLOCKED,
                        LOCKED_HAS_WAITERS,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    wait(&self.state, LOCKED_HAS_WAITERS);
                }
            }
        }
        self.set_acquired();
        if self.is_poisoned() {
            return Err(LockError::Poisoned(PoisonedLock {
                guard: MutexGuard { lock: self },
            }));
        }

        Ok(MutexGuard { lock: self })
    }
    pub fn try_lock(&self) -> LockResult<MutexGuard<'_, T>> {
        _ = self.acquired_check()?;
        if self
            .state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.set_acquired();
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

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    #[inline]
    fn acquired_check<Guard>(&self) -> Result<(), LockError<Guard>> {
        if ACQUIRED_BY_THREAD.with(|acquired_lock| acquired_lock.borrow().contains(&self.mutex_id))
        {
            return Err(LockError::WouldBlock);
        }
        Ok(())
    }

    #[inline]
    fn set_acquired(&self) {
        ACQUIRED_BY_THREAD.with(|acquired_lock| acquired_lock.borrow_mut().insert(self.mutex_id));
    }

    #[inline]
    fn remove_acquired(&self) {
        ACQUIRED_BY_THREAD.with(|acquired_lock| acquired_lock.borrow_mut().remove(&self.mutex_id));
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
        self.lock.remove_acquired();
        if self.lock.state.swap(UNLOCKED, Ordering::Release) == LOCKED_HAS_WAITERS {
            wake_one(&self.lock.state);
        }
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
    use crate::mutex_v2::{LockError, Mutex};

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
    fn test_unlocking_after_panic() {
        let mutex = std::sync::Arc::new(Mutex::new(0u8));
        {
            let mutex = mutex.clone();
            let handle = std::thread::spawn(move || {
                let mut lock = mutex.lock().unwrap();
                *lock += 100;
                panic!("accidentally panicked");
                *lock += 100
            });
            let _ = handle.join();
        }
        let lock_attempt_one = mutex.lock();
        assert!(lock_attempt_one.is_err());
        assert!(mutex.is_poisoned());
        mutex.clear_poison();
        drop(lock_attempt_one);
        let lock_attempt_two = mutex.lock();
        assert!(lock_attempt_two.is_ok());
        assert_eq!(*lock_attempt_two.unwrap(), 100);
    }

    #[test]
    fn test_contented() {
        let m = Mutex::new(1);
        std::hint::black_box(&m);
        let start = std::time::Instant::now();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..1500000 {
                        *m.lock().unwrap() += 1;
                    }
                });
            }
        });
        let duration = start.elapsed();
        println!("locked {} times in {:?}", *m.lock().unwrap(), duration);
    }
}
