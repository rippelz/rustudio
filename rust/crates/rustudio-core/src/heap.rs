//! Giving freed memory back to the system.
//!
//! glibc keeps what a program frees: after a place load the thousands of
//! small blocks behind it (the file's property tables before they are shared,
//! decoded images, asset bytes) stayed in the process for good. On Linux the
//! editor held the same 1.8 GB with the compact property store as without
//! it, where Windows fell by 1 GB. `malloc_trim` returns the free pages; it
//! is called at the two moments a lot was just freed.

/// Returns free heap pages to the system (glibc only; nothing elsewhere,
/// where the allocator does it by itself).
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: malloc_trim only releases memory the allocator holds free.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// Called every frame with whether assets are still loading: trims once,
/// a second after a burst of loading ends.
pub fn settle(busy: bool) {
    use std::sync::Mutex;
    use std::time::Instant;
    static STATE: Mutex<(bool, Option<Instant>)> = Mutex::new((false, None));
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if busy {
        *s = (true, None);
    } else if s.0 {
        match s.1 {
            None => s.1 = Some(Instant::now()),
            Some(t) if t.elapsed().as_secs_f32() >= 1.0 => {
                *s = (false, None);
                drop(s);
                trim();
            }
            Some(_) => {}
        }
    }
}
