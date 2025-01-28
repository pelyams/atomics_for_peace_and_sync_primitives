use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::Ordering;
use thread_id;

struct SpinLock<T> {
    locking_thread: std::sync::atomic::AtomicUsize,
    data: UnsafeCell<T>
}

impl<T> SpinLock<T> {
pub fn new(data: T) -> SpinLock<T> {
    SpinLock {
        locking_thread: std::sync::atomic::AtomicUsize::new(0),
        data: data.into(),
    }
}

pub fn lock(&self) -> SpinGuard<T> {
    let current_thread = thread_id::get();
    loop {
        let state = self.locking_thread.load(Ordering::Relaxed);
        if state == current_thread {
            panic!("This spinlock is not supposed for re-entrance");
        }
        match state {
            0 if self.locking_thread.compare_exchange_weak(
                0,
                current_thread,
                Ordering::Acquire,
                Ordering::Relaxed,
            ).is_ok() => break,
            current_thread=> panic!("This spinlock is not supposed for re-entrance"),
            _ => std::hint::spin_loop(),
            }
        }
        SpinGuard { sl: self }
    }
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

struct SpinGuard<'a, T> {
    sl: &'a SpinLock<T>,
}

impl<T> SpinGuard<'_, T> {
    pub fn unlock(self) {
        drop(self);
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.sl.locking_thread.store(0, Ordering::Release);
    }
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.sl.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.sl.data.get() }
    }
}

impl<T> !Send for SpinGuard<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use std::{sync::Arc, thread};

    #[test]
    fn basic_lock_functionality() {
        let spinlock = SpinLock::new(4u8);
        let locked = spinlock.lock();
        assert_eq!(*locked, 4);
        drop(locked);
        let mut locked = spinlock.lock();
        *locked *= 2;
        assert_eq!(*locked, 8);
    }

    #[test]
    fn concurrent_access() {
        let spinlock = Arc::new(SpinLock::new(String::from("")));
        let _spinlock = spinlock.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_secs(1));
            let mut lock = _spinlock.lock();
            (*lock).push_str("race");
        });
        let mut lock = spinlock.lock();
        (*lock).push_str("race");
        lock.unlock();
        handle.join().unwrap();
        let lock = spinlock.lock();
        assert_eq!(*lock, "racerace");
    }

    #[test]
    #[should_panic]
    fn reentrancy() {
        let spinlock = SpinLock::new("try to reenter me");
        let _first_lock = spinlock.lock();
        let _second_lock = spinlock.lock();
    }

    #[test]
    fn test_mutex_interrupt() {
        let spinlock = Arc::new(SpinLock::new(0));
        let _spinlock = spinlock.clone();
        let handle = thread::spawn(move || {
            let mut guard = _spinlock.lock();
            thread::sleep(Duration::from_secs(1));
            *guard += 1;
            std::panic!();
        });
        thread::sleep(Duration::from_millis(100));
        let _ = handle.join();
        let mut guard = spinlock.lock();
        *guard += 1;
        assert_eq!(*guard, 2);
    }
}
