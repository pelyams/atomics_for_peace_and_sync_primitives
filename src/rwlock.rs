use std::cell::UnsafeCell;
use std::fmt;
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use atomic_wait::{wait, wake_one, wake_all};
use std::cell::RefCell;
use std::collections::{HashSet};

thread_local! {
    static ACQUIRED_BY_THREAD: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

pub type LockResult<Guard> = Result<Guard, RwLockError<Guard>>;

#[derive(Debug)]
pub struct RwLock<T> {
    rwlock_id: u64,
    data: UnsafeCell<T>,
    writers_number: AtomicU32,
    waiting_writers: AtomicU32,
    state: AtomicU32,
    poisoned: UnsafeCell<bool>,
}


impl<T> RwLock<T> {
    pub fn new(data: T) -> RwLock<T> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        RwLock {
            rwlock_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            data: data.into(),
            writers_number: AtomicU32::new(0),
            waiting_writers: AtomicU32::new(0),
            state: AtomicU32::new(0),
            poisoned: false.into(),
        }
    }
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        _ = self.acquired_check()?;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state != u32::MAX {
                if state == u32::MAX - 1 {
                    return Err(RwLockError::TooManyReaders)
                }
                while state & 1 == 1 {
                    wait(&self.state, state);
                    state = self.state.load(Ordering::Relaxed);
                }
                match self.state.compare_exchange_weak(state,
                                                       state + 2,
                                                       Ordering::Acquire,
                                                       Ordering::Relaxed) {
                    Ok(_) => {
                        self.set_acquired();
                        if self.is_poisoned() {
                            return Err(RwLockError::Poisoned(PoisonedLock {
                                guard: RwLockReadGuard { lock: self },
                            }));
                        }
                        return Ok(RwLockReadGuard { lock: self })
                    },
                    Err(changed_state) => state = changed_state,
                }
            } else {
                wait(&self.state, u32::MAX);
            }
        }
    }

    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        _ = self.acquired_check()?;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state <= 1 {
                match self.state.compare_exchange(state, u32::MAX, Ordering::Acquire, Ordering::Relaxed) {
                    Ok(_) => break,
                    Err(updated_state) => { state = updated_state; continue; }
                }
            }
            if state & 1 == 0 {
                match self.state.compare_exchange(state, state + 1, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => (),
                    Err(updated_state) => { state = updated_state; continue; },
                }
            }
            let _waiting_guard = WaitingWriterGuard::new(self);
            let wc = self.writers_number.load(Ordering::Acquire);
            wait(&self.writers_number, wc);

        }
        self.set_acquired();
        if self.is_poisoned() {
            return Err(RwLockError::Poisoned(PoisonedLock {
                guard: RwLockWriteGuard { lock: self },
            }));
        }
        Ok(RwLockWriteGuard { lock: self })
    }

    pub fn try_write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        _ = self.acquired_check()?;
        let state = self.state.load(Ordering::Relaxed);
        if state <= 1 {
            if self.state.compare_exchange(state, u32::MAX, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                self.set_acquired();
                if self.is_poisoned() {
                    return Err(RwLockError::Poisoned(PoisonedLock{
                        guard: RwLockWriteGuard { lock: self },
                    }));
                }
                return Ok(RwLockWriteGuard { lock: self });
            }
        }
        Err(RwLockError::WouldBlock)
    }

    pub fn try_read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        _ = self.acquired_check()?;
        let state = self.state.load(Ordering::Relaxed);
        if state == u32::MAX - 1 {
            return Err(RwLockError::TooManyReaders);
        }
        if state & 1 == 1 {
            return Err(RwLockError::WouldBlock);
        }
        match self.state.compare_exchange(
            state,
            state + 2,
            Ordering::Acquire,
            Ordering::Relaxed
        ) {
            Ok(_) => {
                self.set_acquired();
                if self.is_poisoned() {
                    return Err(RwLockError::Poisoned(PoisonedLock {
                        guard: RwLockReadGuard { lock: self },
                    }));
                }
                Ok(RwLockReadGuard { lock: self })
            },
            Err(_) => Err(RwLockError::WouldBlock)
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }

    pub fn is_poisoned(&self) -> bool {
        unsafe { *self.poisoned.get() }
    }
    pub fn clear_poison(&self) {
        unsafe { *self.poisoned.get() = false };
    }

    #[inline]
    fn acquired_check<Guard>(&self) -> Result<(), RwLockError<Guard>> {
        if ACQUIRED_BY_THREAD.with(
            |acquired_lock| acquired_lock.borrow().contains(&self.rwlock_id)
        ) {
            return Err(RwLockError::WouldBlock);
        }
        Ok(())
    }

    #[inline]
    fn set_acquired(&self) {
        ACQUIRED_BY_THREAD.with(
            |acquired_lock| acquired_lock.borrow_mut().insert(self.rwlock_id)
        );
    }

    #[inline]
    fn remove_acquired(&self) {
        ACQUIRED_BY_THREAD.with(
            |acquired_lock| acquired_lock.borrow_mut().remove(&self.rwlock_id)
        );
    }
}

unsafe impl<T: Send> Send for RwLock<T> {}
unsafe impl<T: Send> Sync for RwLock<T> {}

#[derive(Debug)]
pub struct RwLockWriteGuard<'a, T> {
    lock: &'a RwLock<T>,
}

impl<T> RwLockWriteGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe { *self.lock.poisoned.get() = true };
        }
        self.lock.remove_acquired();
        self.lock.writers_number.fetch_add(1, Ordering::Release);
        self.lock.state.store(0, Ordering::Release);
        if self.lock.waiting_writers.load(Ordering::Acquire) != 0 {
            wake_one(&self.lock.writers_number);
        } else {
            wake_all(&self.lock.state);
        }
    }
}

impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> !Send for RwLockWriteGuard<'_, T> {}


#[derive(Debug)]
pub struct RwLockReadGuard<'a, T> {
    lock: &'a RwLock<T>,
}

impl<T> RwLockReadGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.remove_acquired();
        if self.lock.state.fetch_sub(2, Ordering::Release) == 3 {
            wake_one(&self.lock.writers_number);
        }
    }
}

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> !Send for RwLockReadGuard<'_, T> {}

struct WaitingWriterGuard<'a, T> {
    lock: &'a RwLock<T>,
}

impl<'a, T> WaitingWriterGuard<'a, T> {
    fn new(lock: &'a RwLock<T>) -> Self {
        lock.waiting_writers.fetch_add(1, Ordering::AcqRel);
        Self { lock }
    }
}

impl<'a, T> Drop for WaitingWriterGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.waiting_writers.fetch_sub(1, Ordering::AcqRel);
    }
}


#[derive(Debug)]
pub enum RwLockError<Guard> {
    WouldBlock,
    TooManyReaders,
    Poisoned(PoisonedLock<Guard>),
}

impl<T> PartialEq for RwLockError<T> {
    fn eq(&self, other: &Self) -> bool {
        match self {
            RwLockError::WouldBlock => {
                if let RwLockError::WouldBlock = other {
                    true
                } else {
                    false
                }
            },
            RwLockError::TooManyReaders => {
                if let RwLockError::TooManyReaders = other {
                    true
                } else {
                    false
                }
            },
            RwLockError::Poisoned(_) => {
                if let RwLockError::Poisoned(_) = other {
                    true
                } else {
                    false
                }
            }
        }
    }
}

impl<T> fmt::Display for RwLockError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RwLockError::WouldBlock => write!(f, "Error: Can cause blocking"),
            RwLockError::TooManyReaders => write!(f, "Error: RwLock has too many readers"),
            RwLockError::Poisoned(_) => write!(f, "Error: RwLock is poisoned"),
        }
    }
}

pub struct PoisonedLock<Guard> {
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
    use super::RwLock;
    use crate::rwlock::RwLockError;


    #[test]
    fn test_basic_locking() {
        let rw_lock = RwLock::new(4u8);
        let locked = rw_lock.write().unwrap();
        assert_eq!(*locked, 4);
        drop(locked);
        let mut locked = rw_lock.write().unwrap();
        *locked *= 20;
        assert_eq!(*locked, 80);
    }


    #[test]
    fn test_reentrancy() {
        let rw_lock = RwLock::new("shared data");
        let first_lock = rw_lock.read();
        let second_lock = rw_lock.write();
        assert!(first_lock.is_ok());
        assert!(second_lock.is_err());
        assert_eq!(second_lock.unwrap_err(), RwLockError::WouldBlock);
    }

    #[test]
    fn try_read_fails_when_locked_for_write() {
        let lock = RwLock::new(42);
        let _guard = lock.write().unwrap();
        assert!(matches!(lock.try_read(), Err(RwLockError::WouldBlock)));
    }
}