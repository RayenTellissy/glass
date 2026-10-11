//! FFmpeg log messages as `tracing` events with target `ffmpeg`.
//!
//! Level mapping: panic, fatal and error → `ERROR`; warning → `WARN`; info → `DEBUG`
//! (FFmpeg's info level describes streams and settings, which is noise at our `INFO`);
//! verbose → `TRACE`. FFmpeg's debug and trace levels are not requested at all, they log
//! per packet. `GLASS_LOG=ffmpeg=trace` shows everything that is forwarded.
//!
//! FFmpeg sometimes logs one line in several calls; fragments are joined per thread and
//! emitted when the newline arrives.

use std::cell::RefCell;

use tracing::Level;

pub const TARGET: &str = "ffmpeg";

// FFmpeg's AV_LOG_* values (libavutil/log.h).
const AV_LOG_ERROR: i32 = 16;
const AV_LOG_WARNING: i32 = 24;
const AV_LOG_INFO: i32 = 32;
const AV_LOG_VERBOSE: i32 = 40;

/// Longest partial line kept while waiting for its newline.
const MAX_PENDING: usize = 4096;

/// The `tracing` level for an FFmpeg level, or `None` when it is not forwarded.
pub fn level_for(av_level: i32) -> Option<Level> {
    if av_level <= AV_LOG_ERROR {
        Some(Level::ERROR)
    } else if av_level <= AV_LOG_WARNING {
        Some(Level::WARN)
    } else if av_level <= AV_LOG_INFO {
        Some(Level::DEBUG)
    } else if av_level <= AV_LOG_VERBOSE {
        Some(Level::TRACE)
    } else {
        None
    }
}

/// Whether the current subscriber would record a message at this FFmpeg level. Checked
/// before FFmpeg formats the message, so filtered messages cost almost nothing.
pub(crate) fn enabled(av_level: i32) -> bool {
    match level_for(av_level) {
        Some(Level::ERROR) => tracing::enabled!(target: TARGET, Level::ERROR),
        Some(Level::WARN) => tracing::enabled!(target: TARGET, Level::WARN),
        Some(Level::INFO) => tracing::enabled!(target: TARGET, Level::INFO),
        Some(Level::DEBUG) => tracing::enabled!(target: TARGET, Level::DEBUG),
        Some(Level::TRACE) => tracing::enabled!(target: TARGET, Level::TRACE),
        None => false,
    }
}

thread_local! {
    static PENDING: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn emit(av_level: i32, component: Option<&str>, fragment: &str) {
    let Some(level) = level_for(av_level) else {
        return;
    };
    let line = PENDING.with_borrow_mut(|pending| {
        pending.push_str(fragment);
        if pending.ends_with('\n') || pending.len() >= MAX_PENDING {
            Some(std::mem::take(pending))
        } else {
            None
        }
    });
    let Some(line) = line else {
        return;
    };
    let message = line.trim_end();
    if message.is_empty() {
        return;
    }
    let component = component.unwrap_or("");
    macro_rules! event {
        ($level:expr) => {
            tracing::event!(target: TARGET, $level, component, "{message}")
        };
    }
    match level {
        Level::ERROR => event!(Level::ERROR),
        Level::WARN => event!(Level::WARN),
        Level::INFO => event!(Level::INFO),
        Level::DEBUG => event!(Level::DEBUG),
        Level::TRACE => event!(Level::TRACE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(Level, String)>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, field: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if field.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
            }
            if event.metadata().target() == TARGET {
                let mut m = Message(String::new());
                event.record(&mut m);
                if let Ok(mut events) = self.0.lock() {
                    events.push((*event.metadata().level(), m.0));
                }
            }
        }
    }

    #[test]
    fn ffmpeg_log_reaches_tracing() {
        crate::init();
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry()
            .with(capture.clone())
            .with(tracing_subscriber::filter::LevelFilter::DEBUG);
        tracing::subscriber::with_default(subscriber, || {
            crate::ffi::av_log_for_test(16, "an error\n");
            crate::ffi::av_log_for_test(24, "a warning in ");
            crate::ffi::av_log_for_test(24, "two parts\n");
            crate::ffi::av_log_for_test(32, "info\n");
            // Verbose maps to TRACE, which this subscriber filters out.
            crate::ffi::av_log_for_test(40, "verbose\n");
        });
        let events = capture.0.lock().map(|e| e.clone()).unwrap_or_default();
        assert_eq!(
            events,
            [
                (Level::ERROR, "an error".to_string()),
                (Level::WARN, "a warning in two parts".to_string()),
                (Level::DEBUG, "info".to_string()),
            ]
        );
    }

    #[test]
    fn level_mapping() {
        assert_eq!(level_for(0), Some(Level::ERROR));
        assert_eq!(level_for(8), Some(Level::ERROR));
        assert_eq!(level_for(16), Some(Level::ERROR));
        assert_eq!(level_for(24), Some(Level::WARN));
        assert_eq!(level_for(32), Some(Level::DEBUG));
        assert_eq!(level_for(40), Some(Level::TRACE));
        assert_eq!(level_for(48), None);
        assert_eq!(level_for(56), None);
    }
}
