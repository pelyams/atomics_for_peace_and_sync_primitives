use crate::utils::ring_buffer::RingBuffer;
use atomic_wait::{wait, wake_one};
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

static MAX_SENDER_COUNT: usize = usize::MAX / 2;

struct Channel<T, const N: usize> {
    data: UnsafeCell<RingBuffer<T, N>>,
    senders_count: AtomicUsize,
    sender_wait_num: AtomicU32,
    receiver_state: AtomicU32,
}

impl<T, const N: usize> Channel<T, N> {
    fn new() -> Self {
        Channel {
            data: UnsafeCell::new(RingBuffer::new()),
            senders_count: AtomicUsize::new(1),
            sender_wait_num: AtomicU32::new(0),
            receiver_state: AtomicU32::new(1),
        }
    }

    pub fn channel<'a>(&mut self) -> (Sender<T, N>, Receiver<T, N>) {
        *self = Self::new();
        (
            Sender { channel: self },
            Receiver {
                channel: self,
                _phantom_data: PhantomData,
            },
        )
    }
}

struct Sender<'a, T, const N: usize> {
    channel: &'a Channel<T, N>,
}

impl<T, const N: usize> Sender<'_, T, N> {
    pub fn send(&self, message: T) -> Result<(), SendError<T>> {
        let mut message = message;
        loop {
            if self.channel.receiver_state.load(Ordering::Relaxed) == 0 {
                return Err(SendError::Disconnected(message));
            }
            match unsafe { self.channel.data.get().as_ref().unwrap() }.push(message) {
                Ok(()) => {
                    let mut recv_num = self.channel.receiver_state.load(Ordering::Relaxed);
                    if recv_num != 1 {
                        if self.channel.receiver_state.compare_exchange(2,1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                            wake_one(&self.channel.receiver_state);
                        }
                    }
                    return Ok(());
                }
                Err(m) => {
                    message = m;

                    let sender_wait_num = self.channel.sender_wait_num.load(Ordering::Relaxed);
                    wait(&self.channel.sender_wait_num, sender_wait_num);
                }
            }
        }
    }

    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        if self.channel.receiver_state.load(Ordering::Relaxed) == 0 {
            return Err(TrySendError::Disconnected(message));
        }
        match unsafe { self.channel.data.get().as_ref().unwrap() }.try_push(message) {
            Ok(()) => {
                let mut recv_num = self.channel.receiver_state.load(Ordering::Relaxed);
                if recv_num != 1 {
                    if self.channel.receiver_state.compare_exchange(2,1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                        wake_one(&self.channel.receiver_state);
                    }
                }
                Ok(())
            }
            Err(m) => Err(TrySendError::BufferFull(m)),
        }
    }

    pub fn clone(this: &Self) -> Sender<T, N> {
        //ordering?
        let sender_count = this.channel.senders_count.fetch_add(1, Ordering::Relaxed);
        assert!(sender_count <= MAX_SENDER_COUNT);

        Sender {
            channel: this.channel,
        }
    }
}

impl<T, const N: usize> Drop for Sender<'_, T, N> {
    fn drop(&mut self) {
        //ordr
        self.channel.senders_count.fetch_sub(1, Ordering::Release);
    }
}

impl<T, const N: usize> Clone for Sender<'_, T, N> {
    fn clone(&self) -> Self {
        //ordering
        let sender_count = self.channel.senders_count.fetch_add(1, Ordering::Relaxed);
        assert!(sender_count <= MAX_SENDER_COUNT);

        Sender {
            channel: self.channel,
        }
    }
}

struct Receiver<'a, T, const N: usize> {
    channel: &'a Channel<T, N>,
    _phantom_data: std::marker::PhantomData<*const T>,
}

impl<T, const N: usize> Receiver<'_, T, N> {
    pub fn recv(&self) -> Result<T, ReceiveError> {
        //acquire?
        loop {
            if self.channel.senders_count.load(Ordering::Relaxed) == 0 {
                if let Some(message) = unsafe { self.channel.data.get().as_ref().unwrap() }.try_pop() {
                    return Ok(message);
                }
                return Err(ReceiveError::Disconnected);
            }
            match unsafe { self.channel.data.get().as_ref().unwrap()  }.pop() {
                Some(message) => {
                    //wake sender if any sleeps
                    //CHECK
                    //especially when no senders :)
                    {
                        self.channel.sender_wait_num.fetch_add(1, Ordering::Relaxed);
                        wake_one(&self.channel.sender_wait_num);
                    }

                    return Ok(message);
                }
                None => {
                    //orderign?
                    self.channel.receiver_state.store(2, Ordering::Relaxed);
                    wait(&self.channel.receiver_state, 2);
                }
            }
        }
    }

    pub fn try_recv(&self) -> Result<T, TryReceiveError> {

        unsafe { self.channel.data.get().as_ref().unwrap()  }.try_pop().map_or_else(
            || {
                if self.channel.senders_count.load(Ordering::Relaxed) == 0 {
                    Err(TryReceiveError::Disconnected)
                } else {
                    Err(TryReceiveError::BufferEmpty)
                }
            },
            |message| { return Ok(message) },
        )
    }
}

impl<T, const N: usize> Drop for Receiver<'_, T, N> {
    fn drop(&mut self) {
        self.channel.receiver_state.store(0, Ordering::Relaxed);
    }
}

//???
unsafe impl<T: Send, const N: usize> Sync for Channel<T, N> {}
unsafe impl<T: Send, const N: usize> Send for Receiver<'_, T, N> {}

unsafe impl<T: Send, const N: usize> Send for Sender<'_, T, N> {}
unsafe impl<T: Send, const N: usize> Sync for Sender<'_, T, N> {}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum SendError<T> {
    Disconnected(T),
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum TrySendError<T> {
    BufferFull(T),
    Disconnected(T),
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum ReceiveError {
    Disconnected,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum TryReceiveError {
    BufferEmpty,
    Disconnected,
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn basic_send_receive() {
        let mut channel = Channel::<i32, 4>::new();
        let (tx, rx) = channel.channel();

        tx.send(42).unwrap();
        assert_eq!(rx.recv(), Ok(42));
    }

    #[test]
    fn multiple_messages() {
        let mut channel = Channel::<i32, 4>::new();
        let (tx, rx) = channel.channel();

        for i in 0..4 {
            tx.send(i).unwrap();
        }
        for i in 0..4 {
            assert_eq!(rx.recv(), Ok(i));
        }
    }

    #[test]
    fn receiver_gets_buffered_messages_after_senders_dropped() {
        let mut channel = Channel::<i32, 4>::new();
        let (tx, rx) = channel.channel();

        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);

        assert_eq!(rx.recv(), Ok(1));
        assert_eq!(rx.recv(), Ok(2));
        assert_eq!(rx.recv(), Err(ReceiveError::Disconnected));
    }

    #[test]
    fn try_send_full_buffer() {
        let mut channel = Channel::<i32, 2>::new();
        let (tx, rx) = channel.channel();

        assert!(tx.try_send(1).is_ok());
        assert!(tx.try_send(2).is_ok());
        match tx.try_send(3) {
            Err(TrySendError::BufferFull(3)) => (),
            _ => panic!("Should return BufferFull"),
        }

        drop(rx);
        assert!(matches!(tx.try_send(4), Err(TrySendError::Disconnected(4))));
    }

    #[test]
    fn try_recv_empty_buffer() {
        let mut channel = Channel::<i32, 2>::new();
        let (tx, rx) = channel.channel();

        assert!(matches!(rx.try_recv(), Err(TryReceiveError::BufferEmpty)));
        tx.send(42).unwrap();
        assert_eq!(rx.try_recv(), Ok(42));

        drop(tx);
        assert!(matches!(rx.try_recv(), Err(TryReceiveError::Disconnected)));
    }

    #[test]
    fn cloned_senders() {
        let mut channel = Channel::<i32, 4>::new();
        let (tx1, rx) = channel.channel();
        let tx2 = tx1.clone();

        thread::scope(|s| {
            s.spawn(|| tx1.send(1).unwrap());
            s.spawn(|| tx2.send(2).unwrap());
        });

        let mut msgs = vec![rx.recv().unwrap(), rx.recv().unwrap()];
        msgs.sort();
        assert_eq!(msgs, vec![1, 2]);
    }

    #[test]
    fn blocking_send_receive() {
        let mut channel = Channel::<i32, 2>::new();
        let (tx, rx) = channel.channel();

        thread::scope(|s| {
            s.spawn(move || {
                thread::sleep(Duration::from_millis(50));
                tx.send(99).unwrap();
            });
        });
        assert_eq!(rx.recv(), Ok(99));
    }

    #[test]
    fn send_after_receiver_dropped() {
        let mut channel = Channel::<i32, 2>::new();
        let (tx, rx) = channel.channel();
        drop(rx);

        assert!(matches!(tx.send(42), Err(SendError::Disconnected(42))));
    }
}