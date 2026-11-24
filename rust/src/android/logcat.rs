use std::io::{BufRead, BufReader};
use std::os::fd::FromRawFd;

pub(super) fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        redirect_stdio();
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    });
}

struct AndroidLogger;
static LOGGER: AndroidLogger = AndroidLogger;

impl log::Log for AndroidLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            || metadata.target().starts_with("rustudio") && metadata.level() == log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let text = record.args().to_string();
            rustudio_core::diagnostics::record_android(record.level(), record.target(), &text);
            if record.level() <= log::Level::Warn {
                log_line(&format!("{} {}: {text}", record.level(), record.target()));
            }
        }
    }

    fn flush(&self) {}
}

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

const ANDROID_LOG_INFO: i32 = 4;

fn log_line(text: &str) {
    let tag = c"rustudio";
    let clean: String = text.chars().filter(|c| *c != '\0').collect();
    if let Ok(t) = std::ffi::CString::new(clean) {
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe {
            __android_log_write(ANDROID_LOG_INFO, tag.as_ptr(), t.as_ptr());
        }
    }
}

/// stdout and stderr -> logcat, through a pipe read by a thread.
fn redirect_stdio() {
    let mut fds = [0i32; 2];
    // SAFETY: pipe fills the two descriptors; dup2 / close act on them only.
    let (rd, ok) = unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        libc::dup2(fds[1], 1);
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);
        (fds[0], true)
    };
    if !ok {
        return;
    }
    let _ = std::thread::Builder::new().name("logcat".into()).spawn(move || {
        // SAFETY: `rd` is the read end of the pipe made above; nothing else owns it.
        let file = unsafe { std::fs::File::from_raw_fd(rd) };
        let mut reader = BufReader::new(file);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end();
                    if !line.is_empty() {
                        log_line(line);
                        rustudio_core::diagnostics::record_android(log::Level::Info, "stdio", line);
                    }
                }
            }
        }
    });
}
