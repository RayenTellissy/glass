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

use crate::time::Rational;
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

/// The pixel format stored as a plain integer in codec parameters, if it is in range.
fn pixel_from_int(value: c_int) -> ffmpeg::format::Pixel {
    let nb = sys::AVPixelFormat::AV_PIX_FMT_NB as c_int;
    if (0..nb).contains(&value) {
        // SAFETY: AVPixelFormat is a C enum with consecutive values from -1 (NONE) to
        // AV_PIX_FMT_NB, and `value` was checked to lie inside that range.
        let raw = unsafe { std::mem::transmute::<c_int, sys::AVPixelFormat>(value) };
        raw.into()
    } else {
        ffmpeg::format::Pixel::None
    }
}

fn sample_from_int(value: c_int) -> ffmpeg::format::Sample {
    let nb = sys::AVSampleFormat::AV_SAMPLE_FMT_NB as c_int;
    if (0..nb).contains(&value) {
        // SAFETY: as for pixel formats: consecutive values from -1 to AV_SAMPLE_FMT_NB.
        let raw = unsafe { std::mem::transmute::<c_int, sys::AVSampleFormat>(value) };
        raw.into()
    } else {
        ffmpeg::format::Sample::None
    }
}

/// FFmpeg's name for a codec profile (`High`, `Main 10`, `422`), when it has one.
pub(crate) fn profile_name(codec: ffmpeg::codec::Id, profile: c_int) -> Option<&'static str> {
    // SAFETY: returns null or a string from the codec's static profile table.
    unsafe { static_str(sys::avcodec_profile_name(codec.into(), profile)) }
}

/// Rotation in degrees, counter-clockwise, of a display matrix, as FFmpeg computes it.
pub(crate) fn display_rotation(matrix: &[i32; 9]) -> f64 {
    // SAFETY: reads exactly nine i32 values from the borrowed array.
    unsafe { sys::av_display_rotation_get(matrix.as_ptr()) }
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

/// The fields of a stream's `AVStream` and `AVCodecParameters` that `ffmpeg-next` does
/// not expose, copied out.
#[derive(Clone, Debug)]
pub(crate) struct StreamParams {
    pub codec_tag: u32,
    pub profile: c_int,
    pub level: c_int,
    pub bits_per_raw_sample: c_int,
    pub bits_per_coded_sample: c_int,
    pub pixel_format: ffmpeg::format::Pixel,
    pub sample_format: ffmpeg::format::Sample,
    pub width: c_int,
    pub height: c_int,
    /// The stream's sample aspect ratio, or the codec's when the container has none.
    pub sample_aspect_ratio: Rational,
    pub field_order: ffmpeg::FieldOrder,
    pub primaries: ffmpeg::color::Primaries,
    pub transfer: ffmpeg::color::TransferCharacteristic,
    pub matrix: ffmpeg::color::Space,
    pub range: ffmpeg::color::Range,
    pub chroma_location: ffmpeg::chroma::Location,
    pub sample_rate: c_int,
    pub channels: c_int,
    pub channel_layout: Option<String>,
    pub frame_size: c_int,
    pub display_matrix: Option<[i32; 9]>,
    pub mastering_display: bool,
    pub content_light_level: bool,
    pub dovi: bool,
}

pub(crate) fn stream_params(stream: &ffmpeg::format::stream::Stream) -> StreamParams {
    // SAFETY: the Stream borrows a live AVFormatContext, so the AVStream and its codecpar
    // are valid for this call. Side data entries are owned by codecpar.
    unsafe {
        let st = &*stream.as_ptr();
        let par = &*st.codecpar;
        let side = |kind: sys::AVPacketSideDataType| {
            sys::av_packet_side_data_get(par.coded_side_data, par.nb_coded_side_data, kind).as_ref()
        };
        let display_matrix = side(sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX)
            .filter(|sd| sd.size >= 9 * size_of::<i32>() && !sd.data.is_null())
            .map(|sd| std::ptr::read_unaligned(sd.data as *const [i32; 9]));
        let stream_sar = Rational::from(ffmpeg::Rational::from(st.sample_aspect_ratio));
        let codec_sar = Rational::from(ffmpeg::Rational::from(par.sample_aspect_ratio));
        StreamParams {
            codec_tag: par.codec_tag,
            profile: par.profile,
            level: par.level,
            bits_per_raw_sample: par.bits_per_raw_sample,
            bits_per_coded_sample: par.bits_per_coded_sample,
            pixel_format: if par.codec_type == sys::AVMediaType::AVMEDIA_TYPE_VIDEO {
                pixel_from_int(par.format)
            } else {
                ffmpeg::format::Pixel::None
            },
            sample_format: if par.codec_type == sys::AVMediaType::AVMEDIA_TYPE_AUDIO {
                sample_from_int(par.format)
            } else {
                ffmpeg::format::Sample::None
            },
            width: par.width,
            height: par.height,
            sample_aspect_ratio: if stream_sar.is_valid() {
                stream_sar
            } else {
                codec_sar
            },
            field_order: par.field_order.into(),
            primaries: par.color_primaries.into(),
            transfer: par.color_trc.into(),
            matrix: par.color_space.into(),
            range: par.color_range.into(),
            chroma_location: par.chroma_location.into(),
            sample_rate: par.sample_rate,
            channels: par.ch_layout.nb_channels,
            channel_layout: channel_layout_description(&par.ch_layout),
            frame_size: par.frame_size,
            display_matrix,
            mastering_display: side(
                sys::AVPacketSideDataType::AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
            )
            .is_some(),
            content_light_level: side(sys::AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL)
                .is_some(),
            dovi: side(sys::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF).is_some(),
        }
    }
}

/// Timestamps of the first `limit` entries of a stream's seek index, in stream time base.
/// Demuxers that read a full sample table up front (MOV and MP4) index every frame, so
/// this lists frame times without reading any packet; others index keyframes or nothing.
pub(crate) fn index_timestamps(stream: &ffmpeg::format::stream::Stream, limit: usize) -> Vec<i64> {
    // SAFETY: the Stream borrows a live AVFormatContext; the index is only read, and each
    // entry pointer is used before any other call on the stream.
    unsafe {
        let st = stream.as_ptr() as *mut sys::AVStream;
        let count = sys::avformat_index_get_entries_count(st).max(0) as usize;
        (0..count.min(limit))
            .filter_map(|i| sys::avformat_index_get_entry(st, i as c_int).as_ref())
            .map(|e| e.timestamp)
            .collect()
    }
}

/// Tells the demuxer to drop every packet of streams not in `keep` before returning them,
/// so reading one stream does not pay for the others.
pub(crate) fn keep_only_streams(input: &mut ffmpeg::format::context::Input, keep: &[usize]) {
    for index in 0..input.nb_streams() as usize {
        let discard = if keep.contains(&index) {
            sys::AVDiscard::AVDISCARD_DEFAULT
        } else {
            sys::AVDiscard::AVDISCARD_ALL
        };
        if let Some(mut stream) = input.stream_mut(index) {
            // SAFETY: the StreamMut borrows the Input mutably; `discard` is a plain field
            // the demuxer reads on the next av_read_frame.
            unsafe { (*stream.as_mut_ptr()).discard = discard };
        }
    }
}

/// Sets the YUV to RGB matrix (an `AVColorSpace` value, which swscale's `SWS_CS_*`
/// constants share) and the input range of a scaler; the output is full-range RGB.
pub(crate) fn set_scaler_colourspace(
    scaler: &mut ffmpeg::software::scaling::Context,
    matrix: ffmpeg::color::Space,
    full_range_input: bool,
) -> Result<(), ffmpeg::Error> {
    let space: sys::AVColorSpace = matrix.into();
    // SAFETY: sws_getCoefficients returns a pointer to a static table (falling back to
    // the default for unknown values); the scaler is a live context borrowed mutably.
    let ret = unsafe {
        let table = sys::sws_getCoefficients(space as c_int);
        sys::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            table,
            c_int::from(full_range_input),
            table,
            1,
            0,
            1 << 16,
            1 << 16,
        )
    };
    if ret < 0 {
        Err(ffmpeg::Error::from(ret))
    } else {
        Ok(())
    }
}
