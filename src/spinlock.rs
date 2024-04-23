use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::Ordering;
struct SpinLock<T> {
    owner: UnsafeCell<Option<std::thread::ThreadId>>,
    locked: std::sync::atomic::AtomicBool,
    data: UnsafeCell<T>,
}

impl<T> SpinLock<T> {
    pub fn new(data: T) -> SpinLock<T> {
        SpinLock {
            owner: None.into(),
            locked: std::sync::atomic::AtomicBool::new(false),
            data: data.into(),
        }
    }

    pub fn lock(&self) -> SpinGuard<T> {
        let current_thread = std::thread::current().id();
        unsafe {
            if let Some(thread_id) = *self.owner.get() {
                if current_thread == thread_id {
                    panic!("This spinlock is not supposed for re-entrance")
                }
            }
        }
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        unsafe {
            let owner = self.owner.get();
            *owner = Some(current_thread);
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
        unsafe { *(self.sl.owner.get()) = None.into() }
        self.sl.locked.store(false, Ordering::Release);
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
