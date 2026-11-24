//! Keeps Chromium's memory dumps from taking the app down once it has used more
//! than 2 GiB of malloc'd memory.
//!
//! CEF is built against an older glibc, so its malloc dump provider reads the
//! allocator's totals through `mallinfo`, which reports them as `int`. Past
//! 2 GiB they wrap negative, a checked cast in Chromium fails, and the whole
//! app ends with SIGILL on its MemoryInfra thread, hours into a session.
//!
//! This `mallinfo` reads the same totals through `mallinfo2` and caps each at
//! `i32::MAX`. build.rs exports it from the binary, which comes first in the
//! dynamic linker's lookup, so libcef.so (and the CEF subprocesses, which run
//! this same binary on Linux) bind to it instead of glibc's.

/// glibc's `mallinfo`, with each total capped at `i32::MAX` instead of wrapping.
#[unsafe(no_mangle)]
pub extern "C" fn mallinfo() -> libc::mallinfo {
    // SAFETY: mallinfo2 takes no arguments and only reads the allocator's counters.
    let m = unsafe { libc::mallinfo2() };
    capped(&m)
}

fn capped(m: &libc::mallinfo2) -> libc::mallinfo {
    let cap = |v: libc::size_t| libc::c_int::try_from(v).unwrap_or(libc::c_int::MAX);
    libc::mallinfo {
        arena: cap(m.arena),
        ordblks: cap(m.ordblks),
        smblks: cap(m.smblks),
        hblks: cap(m.hblks),
        hblkhd: cap(m.hblkhd),
        usmblks: cap(m.usmblks),
        fsmblks: cap(m.fsmblks),
        uordblks: cap(m.uordblks),
        fordblks: cap(m.fordblks),
        keepcost: cap(m.keepcost),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_past_2_gib_stay_positive() {
        // 3 GiB, served by mmap and never touched, so it costs address space only.
        let size = 3 << 30;
        // SAFETY: a plain allocation, freed below; nothing reads or writes it.
        let block = unsafe { libc::malloc(size) };
        assert!(!block.is_null());
        // SAFETY: as in `mallinfo`.
        let wide = unsafe { libc::mallinfo2() };
        let info = mallinfo();
        // SAFETY: `block` came from malloc above and is freed once.
        unsafe { libc::free(block) };
        assert!(
            wide.hblkhd >= size,
            "glibc counted {} mmapped bytes",
            wide.hblkhd
        );
        assert_eq!(info.hblkhd, libc::c_int::MAX);
        for v in [info.arena, info.hblkhd, info.uordblks, info.fordblks] {
            assert!(v >= 0);
        }
    }

    #[test]
    fn small_totals_are_unchanged() {
        let m = libc::mallinfo2 {
            arena: 1 << 20,
            ordblks: 3,
            smblks: 0,
            hblks: 1,
            hblkhd: 4096,
            usmblks: 0,
            fsmblks: 0,
            uordblks: 1000,
            fordblks: 24,
            keepcost: 8,
        };
        let info = capped(&m);
        assert_eq!(
            (info.arena, info.hblkhd, info.uordblks),
            (1 << 20, 4096, 1000)
        );
    }
}
