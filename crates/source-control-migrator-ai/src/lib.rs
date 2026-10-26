static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, std::sync::atomic::Ordering::Relaxed);
}

macro_rules! log_verbose {
    ($($arg:tt)*) => {
        if $crate::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[verbose] {}", format_args!($($arg)*));
        }
    };
}

pub mod migrate;
pub mod source;

pub type Error = Box<dyn std::error::Error>;
