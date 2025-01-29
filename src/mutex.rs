use std::cell::{UnsafeCell};
use std::fmt;
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::thread::{Thread};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;


thread_local! {
    static OWNED_BY_THREAD: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

type LockResult<Guard> = Result<Guard, LockError<Guard>>;

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;
const QUEUEING: u8 = 2;

#[derive(Debug)]
pub struct SlowerMutex<T> {
    mutex_id: u64,
    data: UnsafeCell<T>,
    state: AtomicU8,
    queue: UnsafeCell<std::collections::VecDeque<Thread>>,
    poisoned: UnsafeCell<bool>,
}

impl<T> SlowerMutex<T> {

    pub fn new(data: T) -> SlowerMutex<T> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        SlowerMutex {
            mutex_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            data: data.into(),
            state: AtomicU8::new(UNLOCKED),
            queue: std::collections::VecDeque::new().into(),
            poisoned: false.into(),
        }
    }
    pub fn lock(&self) -> LockResult<SlowerMutexGuard<'_,T>> {

        if OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow().get(&self.mutex_id).is_some()
        }) == true {
            return Err(LockError::WouldBlock);
        }

        'outer: loop {
            let mut spin_counter = 0;
            while spin_counter < 100 {
                let state = self.state.load(Ordering::Relaxed);
                match state {
                    UNLOCKED => {
                        if self.state.compare_exchange_weak(
                            UNLOCKED,
                            LOCKED,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        ).is_ok() {
                            break 'outer;
                        }
                    },
                    QUEUEING => {
                        if self.state.compare_exchange_weak(
                            QUEUEING,
                            LOCKED,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        ).is_ok() {
                            // or spin further?!
                            break 'outer;
                        }
                    },
                    LOCKED => {
                        spin_counter += 1;
                    },
                    _ => (),
                }
                std::hint::spin_loop();
            }
            if self.state.compare_exchange(
                LOCKED,
                QUEUEING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ).is_ok() {
                let current_thread = std::thread::current();
                unsafe { (*self.queue.get()).push_back(current_thread) };
                std::thread::park();
            }
        }
        OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow_mut().insert(self.mutex_id);
        });
        if self.is_poisoned() {
            return Err(LockError::Poisoned(PoisonedLock {
                guard: SlowerMutexGuard { lock: self },
            }));
        }

        Ok(SlowerMutexGuard { lock: self })
    }
    pub fn is_poisoned(&self) -> bool {
        unsafe { *self.poisoned.get() }
    }
    pub fn clear_poison(&self) {
        unsafe { *self.poisoned.get() = false };
    }
}

unsafe impl<T: Send> Send for SlowerMutex<T> {}
unsafe impl<T: Send> Sync for SlowerMutex<T> {}

#[derive(Debug)]
struct SlowerMutexGuard<'a, T> {
    lock: &'a SlowerMutex<T>,
}

impl<T> SlowerMutexGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for SlowerMutexGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe { *self.lock.poisoned.get() = true };
        }
        OWNED_BY_THREAD.with(|owned_mutexes| {
            owned_mutexes.borrow_mut().remove(&self.lock.mutex_id);
        });

        let queue = self.lock.queue.get();
        let thread_to_unpark = unsafe { (*queue).pop_front() };
        let has_more_waiters = unsafe { !(*queue).is_empty() };
        let new_state = if has_more_waiters { QUEUEING } else { UNLOCKED };

        self.lock.state.store(new_state, Ordering::Release);
        if let Some(t) = thread_to_unpark {
            t.unpark();
        }
    }
}

impl<T> Deref for SlowerMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SlowerMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> !Send for SlowerMutexGuard<'_, T> {}


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
    use crate::mutex::{LockError, SlowerMutex};

    #[test]
    fn test_basic_locking() {
        let mutex = SlowerMutex::new(4u8);
        let locked = mutex.lock().unwrap();
        assert_eq!(*locked, 4);
        drop(locked);
        let mut locked = mutex.lock().unwrap();
        *locked *= 20;
        assert_eq!(*locked, 80);
    }

    #[test]
    fn test_concurrency() {
        let mutex = std::sync::Arc::new(SlowerMutex::new(0u8));
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
        let mutex = SlowerMutex::new("shared data");
        let first_lock = mutex.lock();
        let second_lock = mutex.lock();
        assert!(first_lock.is_ok());
        assert!(second_lock.is_err());
        assert_eq!(second_lock.unwrap_err(), LockError::WouldBlock);
    }

    #[test]
    fn test_unlocking_after_panic() {
        let mutex = std::sync::Arc::new(SlowerMutex::new(0u8));
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

