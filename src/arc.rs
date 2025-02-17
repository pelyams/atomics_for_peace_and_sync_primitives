use std::cell::UnsafeCell;
use std::mem::ManuallyDrop;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_REF_COUNT: usize = usize::MAX / 2;

struct ArcInner<T> {
    strong_count: AtomicUsize,
    ref_count: AtomicUsize,
    data: UnsafeCell<ManuallyDrop<T>>,
}

pub struct Arc<T> {
    inner: std::ptr::NonNull<ArcInner<T>>,
}

impl<T> Arc<T> {
    pub fn new(data: T) -> Self {
        Arc {
            inner: std::ptr::NonNull::from(Box::leak(Box::new(ArcInner {
                strong_count: AtomicUsize::new(1),
                ref_count: AtomicUsize::new(1),
                data: UnsafeCell::new(ManuallyDrop::new(data)),
            }))),
        }
    }

    // std::sync::Arc implementations uses static method with
    //'this: &mut Self', to mitigate method conflict risks if T also has downgrade method.
    // likely, no one will use this implementation, so we may be risk-assertive
    pub fn downgrade(&self) -> Weak<T> {
        let mut weak_count = unsafe { self.inner.as_ref().ref_count.load(Ordering::Relaxed) };
        loop {
            if weak_count == usize::MAX {
                std::hint::spin_loop();
                weak_count = unsafe { self.inner.as_ref().ref_count.load(Ordering::Relaxed) };
                continue;
            }
            assert!(weak_count <= MAX_REF_COUNT);
            match unsafe { self.inner.as_ref() }.ref_count.compare_exchange(
                weak_count,
                weak_count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Weak { inner: self.inner },
                Err(e) => weak_count = e,
            }
        }
    }

    // contrary to downgrade(), clone() method is more likely to be implemented for T
    // so due to Deref coercion clone(&self) may return a cloned underlying value.
    // use of Arc::clone() should be less conflict-prone
    pub fn clone(this: &Self) -> Self {
        if unsafe { this.inner.as_ref() }
            .strong_count
            .fetch_add(1, Ordering::Relaxed)
            > MAX_REF_COUNT
        {
            std::process::abort();
        }
        Arc { inner: this.inner }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        let inner = unsafe { self.inner.as_ref() };
        if inner
            .ref_count
            .compare_exchange(1, usize::MAX, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let unique = inner.strong_count.load(Ordering::Relaxed) == 1;
        inner.ref_count.store(1, Ordering::Release);
        if unique {
            std::sync::atomic::fence(Ordering::Acquire);
            let data = unsafe { &mut *inner.data.get() };
            return Some(data);
        }
        None
    }

    pub fn into_inner(self) -> Option<T> {
        let inner = unsafe { self.inner.as_ref() };
        if inner
            .ref_count
            .compare_exchange(1, usize::MAX, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        if inner.strong_count.load(Ordering::Relaxed) != 1 {
            inner.ref_count.store(1, Ordering::Release);
            return None;
        }
        std::sync::atomic::fence(Ordering::Acquire);
        let data = ManuallyDrop::into_inner(unsafe { inner.data.get().read() });
        // deallocate self without double freeing T:
        unsafe {
            _ = Box::from_raw(self.inner.as_ptr());
        }
        std::mem::forget(self);
        Some(data)
    }
}

impl<T> Clone for Arc<T> {
    fn clone(&self) -> Self {
        if unsafe { self.inner.as_ref() }
            .strong_count
            .fetch_add(1, Ordering::Relaxed)
            > MAX_REF_COUNT
        {
            std::process::abort();
        }
        Arc { inner: self.inner }
    }
}

impl<T> std::ops::Deref for Arc<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*(self.inner.as_ref()).data.get() }
    }
}

impl<T> Drop for Arc<T> {
    fn drop(&mut self) {
        if unsafe { self.inner.as_ref() }
            .strong_count
            .fetch_sub(1, Ordering::AcqRel)
            == 1
        {
            unsafe {
                ManuallyDrop::drop(self.inner.as_mut().data.get_mut());
            }
            drop(Weak { inner: self.inner });
        }
    }
}

unsafe impl<T: Sync + Send> Send for Arc<T> {}
unsafe impl<T: Sync + Send> Sync for Arc<T> {}

pub struct Weak<T> {
    inner: std::ptr::NonNull<ArcInner<T>>,
}

impl<T> Weak<T> {
    pub fn upgrade(&self) -> Option<Arc<T>> {
        //could be simplified with strong_count.fetch_add(1, Release) unless we had to track their overflowing
        let mut strong_refs = unsafe { self.inner.as_ref() }
            .strong_count
            .load(Ordering::Relaxed);
        loop {
            if strong_refs == 0 {
                return None;
            }
            assert!(strong_refs <= MAX_REF_COUNT);
            match unsafe { self.inner.as_ref() }
                .strong_count
                .compare_exchange(
                    strong_refs,
                    strong_refs + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                Ok(_) => return Some(Arc { inner: self.inner }),
                Err(e) => strong_refs = e,
            }
        }
    }
}

impl<T> Drop for Weak<T> {
    fn drop(&mut self) {
        if unsafe { self.inner.as_ref() }
            .ref_count
            .fetch_sub(1, Ordering::Release)
            == 1
        {
            std::sync::atomic::fence(Ordering::Acquire);
            unsafe { drop(Box::from_raw(self.inner.as_ptr())) };
        }
    }
}

unsafe impl<T: Sync + Send> Send for Weak<T> {}
unsafe impl<T: Sync + Send> Sync for Weak<T> {}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn test_arc_basic_functionality() {
        let value = Arc::new(1);
        thread::scope(|s| {
            let value = Arc::clone(&value);
            s.spawn(move || {
                assert_eq!(*value, 1);
            });
        });
        assert_eq!(*value, 1);
    }

    #[test]
    fn test_clone() {
        let value = Arc::new(10);
        let value_clone = Arc::clone(&value);
        assert_eq!(*value, 10);
        assert_eq!(*value_clone, 10);
    }

    #[test]
    fn test_weak_upgrade() {
        let strong = Arc::new(10);
        let weak = strong.downgrade();

        let upgraded = weak.upgrade().unwrap();
        assert_eq!(*upgraded, 10);

        drop(strong);
        drop(upgraded);

        let upgraded = weak.upgrade();
        assert!(upgraded.is_none());
    }

    #[test]
    fn test_get_mut() {
        let mut value = Arc::new(10);

        if let Some(value) = value.get_mut() {
            *value = 20;
        }
        assert_eq!(*value, 20);

         let _value = Arc::clone(&value);
        assert!(value.get_mut().is_none());
    }

    #[test]
    fn test_into_inner() {
        let value = Arc::new(10);
        let value_clone = Arc::clone(&value);

        assert!(Arc::into_inner(value).is_none());

        drop(value_clone);
        let another_value = Arc::new(20);
        assert_eq!(Arc::into_inner(another_value), Some(20));
    }

    struct DropCounter {
        counter: Arc<AtomicUsize>,
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_drop_behavior() {
        let counter = Arc::new(AtomicUsize::new(0));
        let drop_counter = Arc::new(DropCounter {
            counter: Arc::clone(&counter),
        });
        let drop_counter_clone = Arc::clone(&drop_counter);

        drop(drop_counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        drop(drop_counter_clone);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_drop_behavior_into_inner() {
        let drop_counter = Arc::new(AtomicUsize::new(0));
        let arc = Arc::new(DropCounter {
            counter: drop_counter.clone(),
        });
        thread::scope(|s| {
            let arc = Arc::clone(&arc);
            let drop_counter = Arc::clone(&drop_counter);
            s.spawn(move || {
                assert!(arc.into_inner().is_none());
                assert_eq!(drop_counter.load(Ordering::Relaxed), 0);
            });
        });
        assert!(arc.into_inner().is_some());
        assert_eq!(drop_counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_thread_safety() {
        let value = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let value_clone = Arc::clone(&value);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    value_clone.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(value.load(Ordering::SeqCst), 10000);
    }

    #[test]
    fn test_weak_count() {
        let strong0 = Arc::new(10);
        let weak1 = strong0.downgrade();
        let weak2 = strong0.downgrade();

        let strong1 = weak1.upgrade().unwrap();
        assert_eq!(*strong1, 10);

        drop(strong0);

        let strong2 = weak2.upgrade().unwrap();
        assert_eq!(*strong2, 10);

        drop(strong1);
        drop(strong2);

        assert!(weak1.upgrade().is_none());
        assert!(weak2.upgrade().is_none());
    }
}
