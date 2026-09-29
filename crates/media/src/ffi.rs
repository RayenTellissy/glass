//! Every `unsafe` call of the `media` crate. Each function here reads FFmpeg structures
//! that `ffmpeg-next` does not expose and returns plain Rust values, so the rest of the
//! crate stays safe.
//!
//! Soundness rests on two FFmpeg guarantees: the pointers come from live objects owned by
//! the `ffmpeg-next` wrapper passed in (borrowed for the duration of the call), and enum
//! fields hold values FFmpeg itself wrote. Integer fields that hold enum values are range
//! checked before they become Rust enums.

use std::ffi::{CStr, c_char, c_int, c_void};

use ffmpeg::ffi as sys;
use ffmpeg_next as ffmpeg;

// ---- Logging ------------------------------------------------------------------------------

/// Routes FFmpeg's log output into `tracing` (see `crate::log`). FFmpeg's own level is set
/// to verbose; `crate::log` drops what the subscriber would not record before formatting.
pub(crate) fn install_log_bridge() {
    // SAFETY: both calls only store a level and a function pointer in libavutil globals.
    // The callback is thread safe, as FFmpeg requires.
    unsafe {
        sys::av_log_set_level(sys::AV_LOG_VERBOSE);
        sys::av_log_set_callback(Some(log_callback));
    }
}

unsafe extern "C" fn log_callback(
    avcl: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    args: sys::va_list,
) {
    if fmt.is_null() || !crate::log::enabled(level) {
        return;
    }
    let mut line = [0 as c_char; 1024];
    // Zero: format only the message, without FFmpeg's "[name @ 0x...]" prefix.
    let mut print_prefix: c_int = 0;
    // SAFETY: `fmt` and `args` come straight from av_log; the buffer length is passed in
    // and FFmpeg always NUL-terminates within it.
    let written = unsafe {
        sys::av_log_format_line2(
            avcl,
            level,
            fmt,
            args,
            line.as_mut_ptr(),
            line.len() as c_int,
            &mut print_prefix,
        )
    };
    if written < 0 {
        return;
    }
    // SAFETY: av_log_format_line2 wrote a NUL-terminated string into `line`.
    let message = unsafe { CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
    // SAFETY: av_log's context argument is null or points to a struct whose first
    // member is an `AVClass` pointer.
    let component = unsafe { context_name(avcl) };
    crate::log::emit(level, component, &message);
}

/// Logs `message` through `av_log`, as FFmpeg code would.
#[cfg(test)]
pub(crate) fn av_log_for_test(level: c_int, message: &str) {
    let text = std::ffi::CString::new(message).expect("no NUL in test message");
    // SAFETY: "%s" with one NUL-terminated string argument.
    unsafe { sys::av_log(std::ptr::null_mut(), level, c"%s".as_ptr(), text.as_ptr()) };
}

/// The name FFmpeg prints for a log context: the item name (`h264`, `mov,mp4,...`) or the
/// class name.
unsafe fn context_name(avcl: *mut c_void) -> Option<&'static str> {
    if avcl.is_null() {
        return None;
    }
    // SAFETY: see the caller; the first field of every loggable struct is `*const AVClass`.
    unsafe {
        let class = *(avcl as *const *const sys::AVClass);
        if class.is_null() {
            return None;
        }
        let name = match (*class).item_name {
            Some(item_name) => item_name(avcl),
            None => (*class).class_name,
        };
        static_str(name)
    }
}

/// A static C string (FFmpeg name tables) as `&'static str`.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string that is never freed.
unsafe fn static_str(ptr: *const c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

// ---- Formats ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) struct PixFmtInfo {
    pub depth: u8,
    pub planes: usize,
    pub rgb: bool,
    pub alpha: bool,
    pub hardware: bool,
}

pub(crate) fn pix_fmt_info(format: ffmpeg::format::Pixel) -> Option<PixFmtInfo> {
    let id: sys::AVPixelFormat = format.into();
    // SAFETY: av_pix_fmt_desc_get returns null or a pointer into a static table.
    let desc = unsafe { sys::av_pix_fmt_desc_get(id).as_ref() }?;
    // SAFETY: pure function over the same static table.
    let planes = unsafe { sys::av_pix_fmt_count_planes(id) };
    let flag = |f: c_int| desc.flags & f as u64 != 0;
    Some(PixFmtInfo {
        depth: desc.comp[0].depth as u8,
        planes: planes.max(0) as usize,
        rgb: flag(sys::AV_PIX_FMT_FLAG_RGB),
        alpha: flag(sys::AV_PIX_FMT_FLAG_ALPHA),
        hardware: flag(sys::AV_PIX_FMT_FLAG_HWACCEL),
    })
}

pub(crate) fn chroma_location_name(location: ffmpeg::chroma::Location) -> Option<&'static str> {
    if location == ffmpeg::chroma::Location::Unspecified {
        return None;
    }
    // SAFETY: returns null or a static string.
    unsafe { static_str(sys::av_chroma_location_name(location.into())) }
}

fn channel_layout_description(layout: &sys::AVChannelLayout) -> Option<String> {
    if layout.nb_channels <= 0 {
        return None;
    }
    let mut buf = [0 as c_char; 128];
    // SAFETY: `layout` is a valid AVChannelLayout borrowed from a live object; the buffer
    // length is passed in and the output is NUL-terminated when the call succeeds.
    let ret = unsafe { sys::av_channel_layout_describe(layout, buf.as_mut_ptr(), buf.len()) };
    if ret < 0 {
        return None;
    }
    // SAFETY: NUL-terminated by av_channel_layout_describe.
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(text.to_string_lossy().into_owned())
}

// ---- Frames -------------------------------------------------------------------------------

/// `AVFrame.duration`, in the time base of the packets that produced it.
pub(crate) fn frame_duration(frame: &ffmpeg::Frame) -> i64 {
    // SAFETY: the frame wrapper owns a valid AVFrame for the duration of the borrow.
    unsafe { (*frame.as_ptr()).duration }
}

pub(crate) fn frame_channel_layout(frame: &ffmpeg::Frame) -> Option<String> {
    // SAFETY: as above; ch_layout is an inline struct of the frame.
    channel_layout_description(unsafe { &(*frame.as_ptr()).ch_layout })
}

// ---- Containers and streams ---------------------------------------------------------------

/// `AVFormatContext.start_time` in microseconds, when known.
pub(crate) fn format_start_time(input: &ffmpeg::format::context::Input) -> Option<i64> {
    // SAFETY: the Input owns a valid AVFormatContext for the duration of the borrow.
    let start = unsafe { (*input.as_ptr()).start_time };
    (start != sys::AV_NOPTS_VALUE).then_some(start)
}
