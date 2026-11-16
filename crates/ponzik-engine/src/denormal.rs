//! Flush-to-zero for the audio threads.
//!
//! Filters and feedback loops decaying into silence end up in denormal numbers,
//! which some processors handle tens of times slower than normal ones: a mix
//! whose tails ring out could cost more than real time. While a
//! [`FlushDenormals`] guard lives, the current thread treats them as zero
//! (FTZ and DAZ on x86-64, FZ on AArch64); the previous mode comes back when it
//! drops.

/// Keeps denormals flushed to zero on the current thread while it lives.
pub struct FlushDenormals {
    #[cfg_attr(not(any(target_arch = "x86_64", target_arch = "aarch64")), allow(dead_code))]
    previous: u64,
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use std::arch::asm;

    /// MXCSR: flush to zero (bit 15) and denormals are zero (bit 6).
    const FLAGS: u32 = (1 << 15) | (1 << 6);

    pub fn get() -> u64 {
        let mut csr: u32 = 0;
        // SAFETY: stores MXCSR into a local; no other effect.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags)) };
        csr as u64
    }

    pub fn set(value: u64) {
        let csr = value as u32;
        // SAFETY: loads a value read from MXCSR with only the FTZ/DAZ bits changed.
        unsafe { asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags, readonly)) };
    }

    pub fn flushing(value: u64) -> u64 {
        value | FLAGS as u64
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use std::arch::asm;

    /// FPCR: flush to zero (bit 24).
    const FLAGS: u64 = 1 << 24;

    pub fn get() -> u64 {
        let fpcr: u64;
        // SAFETY: reads FPCR; no other effect.
        unsafe { asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags)) };
        fpcr
    }

    pub fn set(value: u64) {
        // SAFETY: writes back a value read from FPCR with only the FZ bit changed.
        unsafe { asm!("msr fpcr, {}", in(reg) value, options(nomem, nostack, preserves_flags)) };
    }

    pub fn flushing(value: u64) -> u64 {
        value | FLAGS
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub fn get() -> u64 {
        0
    }

    pub fn set(_: u64) {}

    pub fn flushing(value: u64) -> u64 {
        value
    }
}

impl FlushDenormals {
    #[inline]
    pub fn new() -> Self {
        let previous = arch::get();
        let flushing = arch::flushing(previous);
        if flushing != previous {
            arch::set(flushing);
        }
        Self { previous }
    }
}

impl Default for FlushDenormals {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FlushDenormals {
    #[inline]
    fn drop(&mut self) {
        if arch::flushing(self.previous) != self.previous {
            arch::set(self.previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn denormals_flush_inside_the_guard_only() {
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        let half = |x: f32| std::hint::black_box(x) * std::hint::black_box(0.5);
        assert!(half(tiny) > 0.0);
        {
            let _guard = FlushDenormals::new();
            assert_eq!(half(tiny), 0.0);
        }
        assert!(half(tiny) > 0.0);
    }
}
