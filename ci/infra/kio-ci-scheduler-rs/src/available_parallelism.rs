//! Native host-parallelism discovery for shell-facing scheduler entry points.

use std::io;

/// Return the positive parallelism available to this scheduler process.
///
/// The standard library accounts for host restrictions such as CPU affinity.
/// An unavailable value remains an error so callers can request an explicit
/// capacity instead of silently using a platform-specific fallback.
pub fn get() -> io::Result<usize> {
    std::thread::available_parallelism().map(usize::from)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_available_parallelism_is_positive() {
        assert!(super::get().unwrap() > 0);
    }
}
