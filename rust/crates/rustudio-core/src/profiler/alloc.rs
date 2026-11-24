//! Bytes allocated per scope: the binaries install `CountingAlloc` as their
//! global allocator. While the profiler records with allocation counting on,
//! each thread tallies the bytes it allocates (one thread-local add); scopes
//! read the tally at begin and end. Off, an allocation costs one relaxed
//! atomic load more than the system allocator's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Counting on (recording, allocation tracking chosen, and a binary that
/// installed `CountingAlloc`).
pub(crate) static ON: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ALLOCATED: Cell<u64> = const { Cell::new(0) };
}

#[inline(always)]
fn add(n: usize) {
    if ON.load(Ordering::Relaxed) {
        ALLOCATED.with(|c| c.set(c.get().wrapping_add(n as u64)));
    }
}

/// This thread's bytes allocated while counting was on.
#[inline]
pub fn thread_allocated() -> u64 {
    ALLOCATED.with(Cell::get)
}

/// Whether a binary installed the counting allocator (the window hides the
/// "Allocated" figures otherwise).
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// The system allocator, counting bytes per thread while the profiler asks.
/// Call `CountingAlloc::mark_installed()` once at startup.
pub struct CountingAlloc;

impl CountingAlloc {
    pub fn mark_installed() {
        INSTALLED.store(true, Ordering::Relaxed);
    }
}

// SAFETY: every call forwards to `System` with the same arguments; the
// counting only touches a const-initialised thread-local Cell (no allocation,
// no destructor), so it can't recurse into the allocator.
unsafe impl GlobalAlloc for CountingAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // (growth counts as new bytes; shrinking frees, it doesn't allocate)
        add(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
