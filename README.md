just fiddling around w/creating sync primitives in rust for no practical use (i'd even caution you against it)

1. spinlock - the most easy one lock, doesn't even return error if someting goes wrong and deals with problems by panicking
2. mutex - regular mutex, based on user-space queue of threads to be parked/unparked
3. advanced_mutex - the same mutex, but based on atomic-wait crate (https://github.com/m-ou-se/atomic-wait) instead. it provides futex-like functionality, therefore, this queue of threads is moved to kernel space.
