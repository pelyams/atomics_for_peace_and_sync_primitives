use std::fmt;
use std::fmt::Formatter;

#[derive(Debug)]
pub enum LockError<Guard> {
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

pub struct PoisonedLock<Guard> {
    pub guard: Guard,
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