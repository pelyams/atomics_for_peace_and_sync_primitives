In this library, I am fiddling with classic synchronization primitives. May contain errors, so I strongly recommend not to use it 

### Spinlock
- A very basic implementation of spinlock
- Has a minimum optimization to conform MESI protocol
- Deals with problems by panicking
- Panics when user tries to re-acquire lock from a thread currently holding lock

For the latter, field _locked: std::atomic::AtomicBool_ is substituted with _locking_thread: std::atomic::AtomicUsize_ and serves both locked state and locking thread (if any) checking. That relies on assumption thread_id's crate thread_id::get() doesn't return 0 as a thread identifier. 🙂

### SlowMutex
- Mutex implementation based on thread parking/unparking in a user-space dequeue
- Unlike spinlock implementation, instead of panicking it returns errors
- Has more of std-like interface and features

Since atomic property should now have 3 states: UNLOCKED, LOCKED and QUEUING, we can't use reentrancy logic from spinlock implementation anymore. Hence, from now on we check when acquiring the lock if thread local (aka LocalKey) flag is set.

### Mutex
- Instead of parking threads in a user-space deque, it relies on futex-ish library atomic_wait library
- Totally duplicates SlowMutex interface

### RwLock
[todo]


### Condvar 
[todo]

### Semaphore
[todo]

### Channel
[todo]

### Arc
[todo]