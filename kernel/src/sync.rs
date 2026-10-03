use crate::cpu;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

// interrupts stay off while held, or an irq on the same core could spin on it forever
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}

pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
    flags: u64,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        let flags = cpu::push_cli();
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // wait on a plain load so waiters share the line instead of fighting for it
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinGuard { lock: self, flags }
    }
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        cpu::pop_flags(self.flags);
    }
}

pub struct IrqCell<T> {
    busy: UnsafeCell<bool>,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqCell<T> {}

impl<T> IrqCell<T> {
    pub const fn new(data: T) -> Self {
        IrqCell {
            busy: UnsafeCell::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let flags = cpu::push_cli();
        unsafe {
            assert!(!*self.busy.get(), "IrqCell entered twice");
            *self.busy.get() = true;
            let out = f(&mut *self.data.get());
            *self.busy.get() = false;
            cpu::pop_flags(flags);
            out
        }
    }
}
