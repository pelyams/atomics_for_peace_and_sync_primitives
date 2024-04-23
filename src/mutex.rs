use std::cell::UnsafeCell;
use std::fmt;
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{Thread, ThreadId};

type LockResult<Guard> = Result<Guard, LockError<Guard>>;

#[derive(Debug)]
struct RegularMutex<T> {
    data: UnsafeCell<T>,
    locked: AtomicBool,
    thread_id: UnsafeCell<Option<ThreadId>>,
    queue: UnsafeCell<std::collections::VecDeque<Thread>>,
    poisoned: UnsafeCell<bool>,
}

impl<T> RegularMutex<T> {
    pub fn new(data: T) -> RegularMutex<T> {
        RegularMutex {
            data: data.into(),
            locked: AtomicBool::new(false),
            thread_id: None.into(),
            queue: std::collections::VecDeque::new().into(),
            poisoned: false.into(),
        }
    }
    pub fn lock(&self) -> LockResult<RegularMutexGuard<'_,T>> {
        let acquiring_thread = std::thread::current().id();
        unsafe {
            if let Some(thread_id) = *self.thread_id.get() {
                if acquiring_thread == thread_id {
                    return Err(LockError::WouldBlock);
                }
            }
        }

        'outer: while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            let mut spin_counter = 0;
            while spin_counter <= 100
            {
                std::hint::spin_loop();
                if self
                    .locked
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok() {
                    break 'outer;
                }
                spin_counter += 1;
            }
            let current_thread = std::thread::current();
            let queue = self.queue.get();
            unsafe { (*queue).push_back(current_thread) };
            std::thread::park();
        }
        unsafe { *self.thread_id.get() = Some(acquiring_thread) };
        if self.is_poisoned() {
            return Err(LockError::Poisoned(PoisonedLock {
                guard: RegularMutexGuard { lock: self },
            }));
        }

        Ok(RegularMutexGuard { lock: self })
    }
    pub fn is_poisoned(&self) -> bool {
        unsafe { *self.poisoned.get() }
    }
    pub fn clear_poison(&self) {
        unsafe { *self.poisoned.get() = false };
    }
}

unsafe impl<T: Send> Send for RegularMutex<T> {}
unsafe impl<T: Send> Sync for RegularMutex<T> {}

#[derive(Debug)]
struct RegularMutexGuard<'a, T> {
    lock: &'a RegularMutex<T>,
}

impl<T> RegularMutexGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for RegularMutexGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe { *self.lock.poisoned.get() = true };
        }
        unsafe {
            let thread_id = self.lock.thread_id.get();
            *thread_id = None;
        }
        self.lock.locked.store(false, Ordering::Release);
        let queue = self.lock.queue.get();
        unsafe {
            let thread = (*queue).pop_front();
            if thread.is_some() {
                thread.unwrap().unpark();
            }
        };
    }
}

impl<T> Deref for RegularMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for RegularMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> !Send for RegularMutexGuard<'_, T> {}


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
    use crate::mutex::{LockError, RegularMutex};

    #[test]
    fn test_basic_locking() {
        let mutex = RegularMutex::new(4u8);
        let locked = mutex.lock().unwrap();
        assert_eq!(*locked, 4);
        drop(locked);
        let mut locked = mutex.lock().unwrap();
        *locked *= 20;
        assert_eq!(*locked, 80);
    }

    #[test]
    fn test_concurrency() {
        let mutex = std::sync::Arc::new(RegularMutex::new(0u8));
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
        let mutex = RegularMutex::new("shared data");
        let first_lock = mutex.lock();
        let second_lock = mutex.lock();
        assert!(first_lock.is_ok());
        assert!(second_lock.is_err());
        assert_eq!(second_lock.unwrap_err(), LockError::WouldBlock);
    }

    #[test]
    fn test_unlocking_after_panic() {
        let mutex = std::sync::Arc::new(RegularMutex::new(0u8));
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
