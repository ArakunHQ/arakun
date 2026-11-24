//! Keeps Chromium's memory dumps from taking the app down once it has used more
//! than 2 GiB of malloc'd memory.
//!
//! CEF is built against an older glibc, so its malloc dump provider reads the
//! allocator's totals through `mallinfo`, which reports them as `int`. It
//! checks that `arena + hblkhd`, added as `int`, and `uordblks` are not
//! negative. Past 2 GiB they wrap, the check fails, and the whole app ends
//! with SIGILL on its MemoryInfra thread, hours into a session.
//!
//! This `mallinfo` reads the same totals through `mallinfo2` and caps them so
//! each, and `arena + hblkhd`, fit in an `int`. build.rs exports it from the
//! binary, which comes first in the
//! dynamic linker's lookup, so libcef.so (and the CEF subprocesses, which run
//! this same binary on Linux) bind to it instead of glibc's.

/// glibc's `mallinfo`, with the totals capped instead of wrapping.
#[unsafe(no_mangle)]
pub extern "C" fn mallinfo() -> libc::mallinfo {
    // SAFETY: mallinfo2 takes no arguments and only reads the allocator's counters.
    let m = unsafe { libc::mallinfo2() };
    capped(&m)
}

fn capped(m: &libc::mallinfo2) -> libc::mallinfo {
    let cap = |v: libc::size_t| libc::c_int::try_from(v).unwrap_or(libc::c_int::MAX);
    let arena = cap(m.arena);
    // Chromium adds these two as `int`, so their sum has to fit as well.
    let hblkhd = cap(m.hblkhd).min(libc::c_int::MAX - arena);
    libc::mallinfo {
        arena,
        ordblks: cap(m.ordblks),
        smblks: cap(m.smblks),
        hblks: cap(m.hblks),
        hblkhd,
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
        assert!(info.arena.checked_add(info.hblkhd).is_some());
        assert_eq!(info.arena + info.hblkhd, libc::c_int::MAX);
        for v in [info.arena, info.hblkhd, info.uordblks, info.fordblks] {
            assert!(v >= 0);
        }
    }

    #[test]
    fn arena_and_mmapped_sum_fits_an_int() {
        // Each fits an `int`, their sum does not.
        let m = libc::mallinfo2 {
            arena: 1_841_729_536,
            hblkhd: 410_333_360,
            uordblks: 1_500_000_000,
            ..zeroed()
        };
        let info = capped(&m);
        assert_eq!(info.arena, 1_841_729_536);
        assert_eq!(info.arena.checked_add(info.hblkhd), Some(libc::c_int::MAX));
        assert_eq!(info.uordblks, 1_500_000_000);
    }

    fn zeroed() -> libc::mallinfo2 {
        libc::mallinfo2 {
            arena: 0,
            ordblks: 0,
            smblks: 0,
            hblks: 0,
            hblkhd: 0,
            usmblks: 0,
            fsmblks: 0,
            uordblks: 0,
            fordblks: 0,
            keepcost: 0,
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
