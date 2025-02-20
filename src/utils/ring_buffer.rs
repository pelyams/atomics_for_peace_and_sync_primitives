use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// sysctl hw.cachelinesize:
pub(crate) const CACHE_LINE_SIZE: usize = 128;

#[repr(C)]
pub struct RingBuffer<T, const N: usize> {
    buffer: [UnsafeCell<MaybeUninit<T>>; N],
    _pad_head: [u8; CACHE_LINE_SIZE - size_of::<AtomicUsize>()],
    head: AtomicUsize,
    _pad_tail: [u8; CACHE_LINE_SIZE - size_of::<AtomicUsize>()],
    tail: AtomicUsize,
}

impl<T, const N: usize> RingBuffer<T, N> {
    pub fn new() -> Self {
        assert_eq!(N % 2, 0, "Size should be power of 2");
        Self {
            buffer: unsafe { [const { UnsafeCell::new(MaybeUninit::<T>::uninit()) }; N] },
            _pad_head: [0; CACHE_LINE_SIZE - size_of::<AtomicUsize>()],
            head: AtomicUsize::new(0),
            _pad_tail: [0; CACHE_LINE_SIZE - size_of::<AtomicUsize>()],
            tail: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        loop {
            let current_tail = self.tail.load(Ordering::Acquire);
            let current_head = self.head.load(Ordering::Relaxed);

            let used_slots = current_head - current_tail;
            if used_slots >= N {
                return Err(value);
            }
            if self
                .head
                .compare_exchange_weak(
                    current_head,
                    current_head + 1,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                let idx = current_head % N;
                unsafe { *self.buffer.get_unchecked(idx).get() = MaybeUninit::new(value) };
                return Ok(());
            }
        }
    }

    pub fn pop(&self) -> Option<T> {
        let mut current_tail = self.tail.load(Ordering::Relaxed);
        loop {
            let current_head = self.head.load(Ordering::Acquire);

            if current_head == current_tail {
                return None;
            }
            let value = unsafe { (*self.buffer.get_unchecked(current_tail & (N - 1)).get()).assume_init_read() };

            match self.tail.compare_exchange_weak(
                current_tail,
                current_tail.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(value),
                Err(new_tail) => {
                    current_tail = new_tail;
                }
            }
        }
    }

    pub fn try_push(&self, value: T) -> Result<(), T> {
        let current_tail = self.tail.load(Ordering::Acquire);
        let current_head = self.head.load(Ordering::Relaxed);

        let used_slots = current_head - current_tail;
        if used_slots >= N {
            return Err(value);
        }
        if self
            .head
            .compare_exchange_weak(
                current_head,
                current_head + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            let idx = current_head % N;
            unsafe { *self.buffer.get_unchecked(idx).get() = MaybeUninit::new(value) };
            return Ok(());
        }
        Err(value)
    }

    pub fn try_pop(&self) -> Option<T> {
        let current_tail = self.tail.load(Ordering::Relaxed);
        let current_head = self.head.load(Ordering::Acquire);

        if current_head == current_tail {
            return None;
        }

        let idx = current_tail % N;
        let value = unsafe { (*self.buffer.get_unchecked(idx).get()).assume_init_read() };

        match self.tail.compare_exchange_weak(
            current_tail,
            current_tail + 1,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => Some(value),
            Err(_) => None,
        }
    }
}

impl<T, const N: usize> Drop for RingBuffer<T, N> {
    fn drop(&mut self) {
        let head = self.head.get_mut();
        let tail = self.tail.get_mut();

        for idx in *tail..*head {
            unsafe {
                (*self.buffer.get_unchecked(idx & (N - 1)).get()).assume_init_drop();
            }
        }
    }
}
unsafe impl<T, const N: usize> Sync for RingBuffer<T, N> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_mpsc_basic() {
        const N: usize = 32;
        let buffer = Arc::new(RingBuffer::<u32, N>::new());

        let producer = std::thread::spawn({
            let buffer = buffer.clone();
            move || {
                for i in 0..8 {
                    while buffer.push(i).is_err() {
                        std::thread::yield_now();
                    }
                }
            }
        });
        producer.join().unwrap();

        let consumer = std::thread::spawn({
            let buffer = buffer.clone();
            move || {
                let mut sum = 0;
                for _ in 0..8 {
                    if let Some(val) = buffer.pop() {
                        println!("popped value: {}", val);
                        sum += val;
                    }
                }
                sum
            }
        });
        let sum = consumer.join().unwrap();
        assert_eq!(sum, (0..8).sum::<u32>());
    }

    #[test]
    fn test_high_contention_mpmc() {
        const N: usize = 64;
        let buffer = Arc::new(RingBuffer::<usize, N>::new());
        let num_producers = 4;
        let num_consumers = 4;
        let total_items = 10000000;
        let start = std::time::Instant::now();
        let producers: Vec<_> = (0..num_producers)
            .map(|_| {
                let buffer = buffer.clone();
                std::thread::spawn(move || {
                    for i in 0..(total_items / num_producers) {
                        while buffer.push(i).is_err() {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        let consumers: Vec<_> = (0..num_consumers)
            .map(|_| {
                let buffer = buffer.clone();
                std::thread::spawn(move || {
                    let mut count = 0;
                    while count < total_items / num_consumers {
                        if let Some(_) = buffer.pop() {
                            count += 1;
                        }
                    }
                })
            })
            .collect();
        for c in consumers {
            c.join().unwrap();
        }

        assert!(buffer.pop().is_none());
        let duration = start.elapsed();
        println!("{:?}", duration);
    }

    #[test]
    fn test_drop_safety() {
        static DROP_COUNTER: AtomicUsize = AtomicUsize::new(0);

        #[derive(Debug)]
        struct Item;
        impl Drop for Item {
            fn drop(&mut self) {
                DROP_COUNTER.fetch_add(1, Ordering::Relaxed);
            }
        }

        let buffer = RingBuffer::<Item, 4>::new();
        buffer.push(Item).unwrap();
        buffer.push(Item).unwrap();
        buffer.push(Item).unwrap();
        buffer.push(Item).unwrap();
        drop(buffer);
        assert_eq!(DROP_COUNTER.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn test_try_push_full() {
        let buffer = RingBuffer::<u32, 2>::new();
        assert!(buffer.try_push(1).is_ok());
        assert!(buffer.try_push(2).is_ok());
        assert!(buffer.try_push(3).is_err()); // Full
    }

    #[test]
    fn test_try_pop_empty() {
        let buffer = RingBuffer::<u32, 2>::new();
        assert!(buffer.try_pop().is_none());
    }

    #[test]
    fn test_high_contention_try_push() {
        let thread_count = 8;
        let item_count = 8;
        let buffer = Arc::new(RingBuffer::<usize, 64>::new());
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let buffer = buffer.clone();
                std::thread::spawn(move || {
                    for i in 0..item_count {
                        while buffer.try_push(i).is_err() {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let mut sum = 0;
        while let Some(val) = buffer.try_pop() {
            sum += val;
        }
        assert_eq!(sum, thread_count * (0..item_count).sum::<usize>());
    }

    #[test]
    fn test_size() {
        let buffer = RingBuffer::<usize, 64>::new();
        println!("{}", std::mem::size_of::<RingBuffer<usize,64>>());
        println!("{}", std::mem::align_of::<RingBuffer<usize,64>>());
    }
}
