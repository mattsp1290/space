//! Apple TN3150's thread-scoped opt-out closes the dataless check/open race.
//! Constants and ABI: Apple's sys/resource.h (darwin-xnu); no global policy is
//! changed. Actual cloud-provider fixtures remain integration validation.
#![allow(unsafe_code)]
use std::{io, marker::PhantomData, rc::Rc};
const MATERIALIZE_DATALESS: i32 = 3;
const THREAD: i32 = 1;
const OFF: i32 = 1;
#[link(name = "System")]
unsafe extern "C" {
    fn getiopolicy_np(iotype: i32, scope: i32) -> i32;
    fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
}
struct Guard {
    previous: i32,
    active: bool,
    _same_thread: PhantomData<Rc<()>>,
}
impl Guard {
    fn enter() -> io::Result<Self> {
        // SAFETY: scalar SDK arguments, thread scope; no pointers or ownership.
        let previous = unsafe { getiopolicy_np(MATERIALIZE_DATALESS, THREAD) };
        if previous < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "Cannot query no-hydration policy: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        // SAFETY: valid documented scalar policy for this calling thread only.
        if unsafe { setiopolicy_np(MATERIALIZE_DATALESS, THREAD, OFF) } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "Cannot establish no-hydration policy: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        Ok(Self {
            previous,
            active: true,
            _same_thread: PhantomData,
        })
    }
    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // SAFETY: previous was returned by the SDK on this same thread. The
        // Rc marker prevents moving the guard to another thread.
        if unsafe { setiopolicy_np(MATERIALIZE_DATALESS, THREAD, self.previous) } != 0 {
            return Err(io::Error::other(format!(
                "Cannot restore thread I/O policy: {}",
                io::Error::last_os_error()
            )));
        }
        self.active = false;
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
pub(crate) fn with_no_hydration<T>(work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let mut guard = Guard::enter()?;
    let result = work();
    // Normal error paths report restoration failures. Panic unwinding uses
    // Drop's best-effort restore; failure leaves the stricter OFF policy.
    guard.restore()?;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn current() -> i32 {
        // SAFETY: same scalar SDK query as the production guard.
        unsafe { getiopolicy_np(MATERIALIZE_DATALESS, THREAD) }
    }
    #[test]
    fn native_guard_sets_and_restores_policy_on_success_and_error() {
        let before = current();
        assert!(before >= 0);
        with_no_hydration(|| {
            assert_eq!(current() & 3, OFF);
            Ok(())
        })
        .unwrap();
        assert_eq!(current(), before);
        assert!(with_no_hydration::<()>(|| Err(io::Error::other("fixture"))).is_err());
        assert_eq!(current(), before);
    }
    #[test]
    fn nested_guard_restores_disabled_outer_policy() {
        with_no_hydration(|| {
            with_no_hydration(|| Ok(()))?;
            assert_eq!(current() & 3, OFF);
            Ok(())
        })
        .unwrap();
    }
}
