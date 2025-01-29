use std::cell::UnsafeCell;
use std::fmt;
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use atomic_wait::{wait, wake_one};
use std::cell::RefCell;
use std::collections::HashSet;

type LockResult<Guard> = Result<Guard, LockError<Guard>>;

thread_local! {
    static OWNED_BY_THREAD: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

#[derive(Debug)]
struct Mutex<T> {
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
        if OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow().get(&self.mutex_id).is_some()
        }) == true {
            return Err(LockError::WouldBlock);
        }
        if self.state.compare_exchange_weak(0, 1, Ordering::Acquire,  Ordering::Relaxed).is_err() {
            let mut counter = 0;
            while counter < 100 && self.state.load(Ordering::Relaxed) != 0 {
                counter += 1;
                std::hint::spin_loop();
            }
            if self.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
                loop {
                    let state = self.state.load(Ordering::Relaxed);
                    if state == 0 {
                        match self.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed) {
                            Ok(_) => break,
                            Err(_) => continue,
                        }
                    }
                    if state != 2 {
                        self.state.store(2, Ordering::Relaxed);
                    }
                    wait(&self.state, 2);
                }
            }
        }
        OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow_mut().insert(self.mutex_id);
        });
        if self.is_poisoned() {
            return Err(LockError::Poisoned(PoisonedLock {
                guard: MutexGuard { lock: self },
            }));
        }

        Ok(MutexGuard { lock: self })
    }
    pub fn is_poisoned(&self) -> bool {
        unsafe { *self.poisoned.get() }
    }
    pub fn clear_poison(&self) {
        unsafe { *self.poisoned.get() = false };
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

#[derive(Debug)]
struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
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
        OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow_mut().remove(&self.lock.mutex_id);
        });
        if self.lock.state.swap(0,Ordering::Release) == 2 {
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


#[derive(Debug)]
enum LockError<Guard> {
    WouldBlock,
    Poisoned(PoisonedLock<Guard>),
}

impl<T> PartialEq for LockError<T> {
    fn eq(&self, other: &Self) -> bool {
        match self {
            LockError::WouldBlock => {
                if let LockError::WouldBlock = other {
                    true
                } else {
                    false
                }
            },
            LockError::Poisoned(_)  => {
                if let LockError::WouldBlock = other {
                    false
                } else {
                    true
                }
            },
        }
    }
}

impl<T> fmt::Display for LockError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LockError::WouldBlock => write!(f, "Error: Can cause blocking"),
            LockError::Poisoned(_) => write!(f, "Error: Lock is poisoned"),
        }
    }
}

struct PoisonedLock<Guard> {
    guard: Guard,
}

impl<T> fmt::Display for PoisonedLock<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Lock is poisoned!")
    }
}

impl<T> fmt::Debug for PoisonedLock<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Lock is poisoned!")
    }
}

#[cfg(test)]
mod tests {
    use crate::advanced_mutex::{LockError, Mutex};

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
}
