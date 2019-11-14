#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
pub mod raw {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::cell::UnsafeCell;

// does this need to be heap allocated to deal with move semantics?
// I would think moving the mutex would break it, but then again
// rust statically guarantees no moves unless we have exclusive ownership
// any reasonable use (Arc<AosMutex>) will require it gets moved
#[derive(Debug)]
pub struct AosMutexRaw {
    m: UnsafeCell<raw::aos_mutex>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[must_use]
pub enum LockSuccessReason {
    Unlocked,
    OwnerDied,
}

/// NOT Re-entrant
impl AosMutexRaw {
    pub fn new() -> Self {
        Self {
            m: unsafe { std::mem::zeroed::<UnsafeCell<raw::aos_mutex>>() },
        }
    }

    pub fn lock(&self) -> LockSuccessReason {
        let ret = unsafe { raw::mutex_grab(self.m.get()) };
        match ret {
            0 => LockSuccessReason::Unlocked,
            1 => LockSuccessReason::OwnerDied,
            _ => unreachable!("AosMutexRaw mutex_grab failed with {}", ret),
        }
    }

    pub fn unlock(&self) {
        unsafe { raw::mutex_unlock(self.m.get()) }
    }

    pub fn try_lock(&self) -> Result<LockSuccessReason, ()> {
        let ret = unsafe { raw::mutex_trylock(self.m.get()) };
        match ret {
            0 => Ok(LockSuccessReason::Unlocked),
            1 => Ok(LockSuccessReason::OwnerDied),
            4 => Err(()),
            _ => unreachable!("AosMutexRaw mutex_trylock failed with {}", ret),
        }
    }
}

impl Drop for AosMutexRaw {
    fn drop(&mut self) {
        if self.try_lock().is_ok() {
            self.unlock()
        }
    }
}

pub struct AosConditionRaw {}
