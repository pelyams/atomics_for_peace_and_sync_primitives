In this library, I am fiddling with classic synchronization primitives. May contain errors, so I strongly recommend not to use it 

### Spinlock
- A very basic implementation of spinlock
- Has a minimum optimization to conform MESI protocol
- Deals with problems by panicking
- Panics when user tries to re-acquire lock from a thread currently holding lock

For the latter, field _locked: std::atomic::AtomicBool_ is substituted with _locking_thread: std::atomic::AtomicUsize_ and serves both locked state and locking thread (if any) checking. That relies on assumption thread_id's crate thread_id::get() doesn't return 0 as a thread identifier. 🙂

### Mutex v.1
- Mutex implementation based on thread parking/unparking in a user-space dequeue
- Unlike spinlock implementation, instead of panicking it returns errors
- Has more of std-like interface and features

### Mutex v.2
- Instead of parking threads in a user-space deque, it relies on futex-ish atomic_wait library
- Totally duplicates Mutex v.1 interface
Since atomic state property should now take one of 3 states: UNLOCKED, LOCKED and LOCKED_HAS_WAITERS, we can't use reentrancy logic from spinlock or mutex_v1 implementations anymore. Hence, from now on we check when acquiring the lock if thread local (aka LocalKey) flag is set.

### RwLock
- Writer-friendly read-write lock
- Prevents unnecessary wake_all() calls for reading threads (as a downside, now it an unpleasant number of atomics inside (namely, 3))

### Condvar 
- A very basic implementation. Might remind of other very basic implementations from corresponding literature

### Semaphore
- Atomic + futex-alike atomic_wait based implementation
Been considering to use guards for acquiring (and returning permits back in Drop implementation, RAII, you know), but ultimately opted off in favour of semaphore flexibility

### Channel
- MPSC buffered channel
- Backed by a ring buffer (and hence channel has a boundary of buffer size being power of 2)

### Arc
- Some regular Arc implementation, a pale imitation of std version.
