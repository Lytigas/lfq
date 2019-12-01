#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
pub mod raw {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
pub mod eos;

use nix::errno::errno;
use std::cell::UnsafeCell;

// does this need to be heap allocated to deal with move semantics?
// I would think moving the mutex would break it, but then again
// rust statically guarantees no moves unless we have exclusive ownership
// any reasonable use (Arc<AosMutex>) will require it gets moved
#[derive(Debug)]
#[repr(transparent)]
pub struct AosMutexRaw {
    m: UnsafeCell<raw::aos_mutex>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[must_use]
pub enum LockSuccessReason {
    Unlocked,
    OwnerDied,
}
impl LockSuccessReason {
    fn ok(self) {}
}

/// NOT Re-entrant
impl AosMutexRaw {
    pub fn new() -> Self {
        Self {
            m: unsafe { std::mem::zeroed() },
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

use lock_api::{GuardNoSend, Mutex, RawMutex};
unsafe impl Send for AosMutexRaw {}
unsafe impl Sync for AosMutexRaw {}
unsafe impl RawMutex for AosMutexRaw {
    type GuardMarker = GuardNoSend;

    /// DO NOT USE!
    const INIT: AosMutexRaw = Self {
        m: UnsafeCell::new(raw::aos_mutex {
            next: 0,
            previous: 0 as *mut _,
            futex: 0,
        }),
    };

    #[inline]
    fn lock(&self) {
        self.lock().ok()
    }

    #[inline]
    fn try_lock(&self) -> bool {
        self.try_lock().is_ok()
    }

    fn unlock(&self) {
        self.unlock()
    }
}

pub type AosMutex<T> = lock_api::Mutex<AosMutexRaw, T>;
pub type AosMutexGuard<'a, T> = lock_api::MutexGuard<'a, AosMutexRaw, T>;

pub enum FutexWakeReason {
    SignalInterrupt,
    Timeout,
    Errno(i32),
}

pub struct AosFutexNotifier {
    m: UnsafeCell<raw::aos_futex>,
}

/// A simple broadcast notifier based on `aos_futex`
impl AosFutexNotifier {
    pub fn new() -> Self {
        Self {
            m: unsafe { std::mem::zeroed() },
        }
    }

    pub fn wait(&self) -> Result<(), FutexWakeReason> {
        let ret = unsafe { raw::futex_wait(self.m.get()) };
        match ret {
            0 => {
                self.unset();
                Ok(())
            }
            1 => Err(FutexWakeReason::SignalInterrupt),
            _ => Err(FutexWakeReason::Errno(errno())),
        }
    }

    pub fn wait_timeout(&self, timeout: std::time::Duration) -> Result<(), FutexWakeReason> {
        let ts = raw::timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: timeout.subsec_nanos().into(),
        };
        let ret = unsafe { raw::futex_wait_timeout(self.m.get(), &ts) };
        match ret {
            0 => {
                self.unset();
                Ok(())
            }
            1 => Err(FutexWakeReason::SignalInterrupt),
            2 => Err(FutexWakeReason::Timeout),
            _ => Err(FutexWakeReason::Errno(errno())),
        }
    }

    /// Returns was not set before
    #[inline(always)]
    fn unset(&self) -> bool {
        unsafe { raw::futex_unset(self.m.get()) != 0 }
    }

    pub fn notify(&self) -> Result<(), i32> {
        let ret = unsafe { raw::futex_set(self.m.get()) };
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        // We didn't wake anyone up, so unset it ourselves
        if ret <= 0 {
            self.unset();
        }
        if ret < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
}

unsafe impl Send for AosFutexNotifier {}
unsafe impl Sync for AosFutexNotifier {}

mod test {
    use super::*;

    #[test]
    fn raw_mutex_const_init_eq_memset() {
        const size: usize = std::mem::size_of::<AosMutexRaw>();
        let init: [u8; size] = unsafe { std::mem::transmute(AosMutexRaw::INIT) };
        let other: [u8; size] = unsafe { std::mem::transmute(AosMutexRaw::new()) };
        assert_eq!(init, other);
    }
}
