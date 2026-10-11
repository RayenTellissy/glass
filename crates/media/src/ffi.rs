//! Every `unsafe` call of the `media` crate. Each function here reads FFmpeg structures
//! that `ffmpeg-next` does not expose and returns plain Rust values, so the rest of the
//! crate stays safe.
//!
//! Soundness rests on two FFmpeg guarantees: the pointers come from live objects owned by
//! the `ffmpeg-next` wrapper passed in (borrowed for the duration of the call), and enum
//! fields hold values FFmpeg itself wrote. Integer fields that hold enum values are range
//! checked before they become Rust enums.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use ffmpeg::ffi as sys;

use crate::time::Rational;
use ffmpeg_next as ffmpeg;

#[cfg(target_os = "macos")]
pub(crate) mod imageio;

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

/// Sets `AVFrame.duration`, in the frame's time base.
pub(crate) fn set_frame_duration(frame: &mut ffmpeg::Frame, duration: i64) {
    // SAFETY: the frame wrapper owns a valid AVFrame; `duration` is a plain field.
    unsafe { (*frame.as_mut_ptr()).duration = duration }
}

/// A new frame referencing the same (reference-counted) buffers as `frame`, without
/// copying pixels. `None` if FFmpeg cannot allocate the frame.
pub(crate) fn frame_ref(frame: &ffmpeg::frame::Video) -> Option<ffmpeg::frame::Video> {
    // SAFETY: av_frame_clone reads a valid AVFrame and returns a new one (or null) that
    // the wrapper then owns and frees.
    unsafe {
        let copy = sys::av_frame_clone(frame.as_ptr());
        (!copy.is_null()).then(|| ffmpeg::frame::Video::wrap(copy))
    }
}

/// The channels of an audio frame in order, as FFmpeg `AVChannel` values (`0` front
/// left, `1` front right, `2` front centre, `3` LFE, ...), or `-1` for each channel of a
/// layout that names only its channel count. Clears `out` first; no allocation once `out`
/// has room.
pub(crate) fn frame_channel_ids(frame: &ffmpeg::Frame, out: &mut Vec<i32>) {
    out.clear();
    // SAFETY: the frame wrapper owns a valid AVFrame; ch_layout is an inline struct whose
    // `order` FFmpeg set, and whose union member matches that order: `mask` for native
    // and ambisonic orders, `map` (nb_channels AVChannelCustom entries, `id` first) for
    // custom order. Channel ids are read as plain integers.
    unsafe {
        let layout = &(*frame.as_ptr()).ch_layout;
        let n = layout.nb_channels.max(0) as usize;
        match layout.order {
            sys::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE => {
                let mask = layout.u.mask;
                out.extend((0..64).filter(|bit| mask & (1u64 << bit) != 0).take(n));
            }
            sys::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM if !layout.u.map.is_null() => {
                let map = layout.u.map;
                out.extend((0..n).map(|i| *(map.add(i) as *const i32)));
            }
            _ => {}
        }
        out.resize(n, -1);
    }
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
    /// Frames of reordering delay (B-frames); zero means presentation order equals
    /// decode order.
    pub video_delay: c_int,
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
            video_delay: par.video_delay,
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

/// One entry of a stream's seek index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    /// In stream time base; MOV and MP4 store decode times here, Matroska presentation
    /// times.
    pub timestamp: i64,
    pub pos: i64,
    pub key: bool,
}

/// Every entry of a stream's seek index.
pub(crate) fn index_entries(stream: &ffmpeg::format::stream::Stream) -> Vec<IndexEntry> {
    // SAFETY: as for `index_timestamps`.
    unsafe {
        let st = stream.as_ptr() as *mut sys::AVStream;
        let count = sys::avformat_index_get_entries_count(st).max(0);
        (0..count)
            .filter_map(|i| sys::avformat_index_get_entry(st, i).as_ref())
            .map(|e| IndexEntry {
                timestamp: e.timestamp,
                pos: e.pos,
                key: e.flags() & sys::AVINDEX_KEYFRAME != 0,
            })
            .collect()
    }
}

/// Seeks to the last keyframe of `stream` whose index timestamp (stream ticks) is at or
/// before `ts` (`avformat_seek_file` with that stream's time base).
pub(crate) fn seek_stream(
    input: &mut ffmpeg::format::context::Input,
    stream: usize,
    ts: i64,
) -> Result<(), ffmpeg::Error> {
    let index = c_int::try_from(stream).map_err(|_| ffmpeg::Error::StreamNotFound)?;
    // SAFETY: the Input owns a valid AVFormatContext, borrowed mutably for the call.
    let ret = unsafe { sys::avformat_seek_file(input.as_mut_ptr(), index, i64::MIN, ts, ts, 0) };
    if ret < 0 {
        Err(ffmpeg::Error::from(ret))
    } else {
        Ok(())
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

/// How swscale reads YUV input: the matrix, the input range and the chroma position in
/// 1/256 luma samples (`None` for a direction without subsampling).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SwsYuv {
    pub matrix: ffmpeg::color::Space,
    pub full_range: bool,
    pub h_chroma_pos: Option<i32>,
    pub v_chroma_pos: Option<i32>,
}

/// Converts a CPU frame to tightly packed 8-bit RGBA, at the same size or `size`, with swscale
/// (bilinear, accurate rounding, full chroma interpolation). For YUV input `matrix` (an
/// `AVColorSpace` value, which swscale's `SWS_CS_*` constants share), the input range and
/// the chroma position (in 1/256 luma samples, `None` for a direction without
/// subsampling) are set before the scaler is initialised, which `sws_getContext` cannot
/// do. RGB input ignores them.
pub(crate) fn scale_to_rgba(
    frame: &ffmpeg::frame::Video,
    yuv: Option<SwsYuv>,
    size: Option<(u32, u32)>,
) -> Result<Vec<u8>, ffmpeg::Error> {
    let (src_width, src_height) = (frame.width(), frame.height());
    let (width, height) = size.unwrap_or((src_width, src_height));
    if width == 0 || height == 0 {
        return Err(ffmpeg::Error::InvalidData);
    }
    let sw = c_int::try_from(src_width).map_err(|_| ffmpeg::Error::InvalidData)?;
    let sh = c_int::try_from(src_height).map_err(|_| ffmpeg::Error::InvalidData)?;
    let w = c_int::try_from(width).map_err(|_| ffmpeg::Error::InvalidData)?;
    let h = c_int::try_from(height).map_err(|_| ffmpeg::Error::InvalidData)?;
    let src_format: sys::AVPixelFormat = frame.format().into();
    let flags = sys::SwsFlags::SWS_BILINEAR as c_uint
        | sys::SwsFlags::SWS_ACCURATE_RND as c_uint
        | sys::SwsFlags::SWS_FULL_CHR_H_INT as c_uint;
    // Rows padded to 64 bytes so swscale's vector paths can write whole blocks.
    let row = width as usize * 4;
    let stride = row.next_multiple_of(64);
    let mut out = vec![0u8; stride * height as usize];
    // SAFETY: the context is allocated, configured through its public fields before
    // sws_init_context (as the API documents), used once and freed on every path. The
    // source pointers and strides come from the live frame; the destination buffer holds
    // `stride * height` bytes, which is what swscale writes for RGBA at this size.
    unsafe {
        let ctx = sys::sws_alloc_context();
        if ctx.is_null() {
            return Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::ENOMEM,
            });
        }
        (*ctx).src_w = sw;
        (*ctx).src_h = sh;
        (*ctx).dst_w = w;
        (*ctx).dst_h = h;
        (*ctx).src_format = src_format as c_int;
        (*ctx).dst_format = sys::AVPixelFormat::AV_PIX_FMT_RGBA as c_int;
        (*ctx).flags = flags;
        if let Some(yuv) = yuv {
            if let Some(p) = yuv.h_chroma_pos {
                (*ctx).src_h_chr_pos = p;
            }
            if let Some(p) = yuv.v_chroma_pos {
                (*ctx).src_v_chr_pos = p;
            }
        }
        let ret = sys::sws_init_context(ctx, std::ptr::null_mut(), std::ptr::null_mut());
        if ret < 0 {
            sys::sws_freeContext(ctx);
            return Err(ffmpeg::Error::from(ret));
        }
        if let Some(SwsYuv {
            matrix, full_range, ..
        }) = yuv
        {
            let space: sys::AVColorSpace = matrix.into();
            let table = sys::sws_getCoefficients(space as c_int);
            let ret = sys::sws_setColorspaceDetails(
                ctx,
                table,
                c_int::from(full_range),
                table,
                1,
                0,
                1 << 16,
                1 << 16,
            );
            if ret < 0 {
                sys::sws_freeContext(ctx);
                return Err(ffmpeg::Error::from(ret));
            }
        }
        let raw = &*frame.as_ptr();
        let dst = [
            out.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ];
        let dst_stride = [stride as c_int, 0, 0, 0];
        let src: [*const u8; 4] = [raw.data[0], raw.data[1], raw.data[2], raw.data[3]];
        let written = sys::sws_scale(
            ctx,
            src.as_ptr(),
            raw.linesize.as_ptr(),
            0,
            sh,
            dst.as_ptr(),
            dst_stride.as_ptr(),
        );
        sys::sws_freeContext(ctx);
        if written != h {
            return Err(ffmpeg::Error::from(written.min(-1)));
        }
    }
    if stride != row {
        for y in 1..height as usize {
            out.copy_within(y * stride..y * stride + row, y * row);
        }
        out.truncate(row * height as usize);
    }
    Ok(out)
}

// ---- Encoding and muxing -----------------------------------------------------------------

/// FFmpeg's colour enums for tag names (`bt709`, `smpte2084`, `bt2020nc`); unknown or
/// missing names stay unspecified.
pub(crate) fn colour_enums(
    primaries: Option<&str>,
    transfer: Option<&str>,
    matrix: Option<&str>,
) -> (
    ffmpeg::color::Primaries,
    ffmpeg::color::TransferCharacteristic,
    ffmpeg::color::Space,
) {
    let c = |name: Option<&str>| name.and_then(|n| std::ffi::CString::new(n).ok());
    // SAFETY: each call reads a NUL-terminated string borrowed for the call and returns
    // an enum value or a negative error; negative values are mapped to unspecified before
    // they become Rust enums, and non-negative ones are valid FFmpeg enum values.
    unsafe {
        let p = c(primaries).map_or(-1, |n| sys::av_color_primaries_from_name(n.as_ptr()));
        let t = c(transfer).map_or(-1, |n| sys::av_color_transfer_from_name(n.as_ptr()));
        let m = c(matrix).map_or(-1, |n| sys::av_color_space_from_name(n.as_ptr()));
        (
            if p < 0 {
                ffmpeg::color::Primaries::Unspecified
            } else {
                std::mem::transmute::<c_int, sys::AVColorPrimaries>(p).into()
            },
            if t < 0 {
                ffmpeg::color::TransferCharacteristic::Unspecified
            } else {
                std::mem::transmute::<c_int, sys::AVColorTransferCharacteristic>(t).into()
            },
            if m < 0 {
                ffmpeg::color::Space::Unspecified
            } else {
                std::mem::transmute::<c_int, sys::AVColorSpace>(m).into()
            },
        )
    }
}

/// Lets an encoder use frame and slice threads, `count` of them (0: one per core).
pub(crate) fn set_encoder_threads(context: &mut ffmpeg::codec::Context, count: usize) {
    // SAFETY: the Context owns a valid, not yet opened AVCodecContext borrowed mutably;
    // both fields are plain integers FFmpeg reads when the codec opens.
    unsafe {
        let ctx = context.as_mut_ptr();
        (*ctx).thread_type = sys::FF_THREAD_FRAME as c_int | sys::FF_THREAD_SLICE as c_int;
        (*ctx).thread_count = c_int::try_from(count).unwrap_or(0);
    }
}

/// Sets the chroma sample location of an encoder before it opens (`ffmpeg-next` has no
/// setter).
pub(crate) fn set_chroma_location(
    context: &mut ffmpeg::codec::Context,
    location: ffmpeg::chroma::Location,
) {
    // SAFETY: the Context owns a valid, not yet opened AVCodecContext borrowed mutably.
    unsafe {
        (*context.as_mut_ptr()).chroma_sample_location = location.into();
    }
}

/// Stores a display matrix (rotation and mirroring) as coded side data of an output
/// stream, which the MOV/MP4 muxer writes into the track header.
pub(crate) fn set_display_matrix(
    output: &mut ffmpeg::format::context::Output,
    stream: usize,
    matrix: &[i32; 9],
) -> Result<(), ffmpeg::Error> {
    let mut st = output
        .stream_mut(stream)
        .ok_or(ffmpeg::Error::StreamNotFound)?;
    // SAFETY: the StreamMut borrows the Output mutably, so its codecpar is valid and not
    // shared; av_packet_side_data_new allocates `size` bytes (or returns null) owned by
    // codecpar, and exactly that many bytes are written.
    unsafe {
        let par = &mut *(*st.as_mut_ptr()).codecpar;
        let size = size_of::<[i32; 9]>();
        let sd = sys::av_packet_side_data_new(
            &mut par.coded_side_data,
            &mut par.nb_coded_side_data,
            sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
            size,
            0,
        );
        if sd.is_null() || (*sd).data.is_null() {
            return Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::ENOMEM,
            });
        }
        std::ptr::copy_nonoverlapping(matrix.as_ptr().cast::<u8>(), (*sd).data, size);
    }
    Ok(())
}

/// `AVMasteringDisplayMetadata` (libavutil/mastering_display_metadata.h), which the
/// generated bindings leave out; its layout has not changed since FFmpeg 3.
#[repr(C)]
struct MasteringDisplayMetadata {
    display_primaries: [[sys::AVRational; 2]; 3],
    white_point: [sys::AVRational; 2],
    min_luminance: sys::AVRational,
    max_luminance: sys::AVRational,
    has_primaries: c_int,
    has_luminance: c_int,
}

/// `AVContentLightMetadata` (libavutil/mastering_display_metadata.h).
#[repr(C)]
struct ContentLightMetadata {
    max_cll: c_uint,
    max_fall: c_uint,
}

/// Stores mastering display and content light levels as coded side data of an output
/// stream, for the muxer to write into the header.
pub(crate) fn set_hdr_metadata(
    output: &mut ffmpeg::format::context::Output,
    stream: usize,
    hdr: &crate::export::HdrMetadata,
) -> Result<(), ffmpeg::Error> {
    let chroma = |v: u16| sys::AVRational {
        num: c_int::from(v),
        den: 50_000,
    };
    let luma = |v: u32| sys::AVRational {
        num: c_int::try_from(v).unwrap_or(c_int::MAX),
        den: 10_000,
    };
    let xy = |(x, y): (u16, u16)| [chroma(x), chroma(y)];
    if let Some(m) = &hdr.mastering {
        let value = MasteringDisplayMetadata {
            display_primaries: [xy(m.red), xy(m.green), xy(m.blue)],
            white_point: xy(m.white),
            min_luminance: luma(m.min_luminance),
            max_luminance: luma(m.max_luminance),
            has_primaries: 1,
            has_luminance: 1,
        };
        add_stream_side_data(
            output,
            stream,
            sys::AVPacketSideDataType::AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
            &value,
        )?;
    }
    if let Some(c) = &hdr.content_light {
        let value = ContentLightMetadata {
            max_cll: c.max_cll.into(),
            max_fall: c.max_fall.into(),
        };
        add_stream_side_data(
            output,
            stream,
            sys::AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
            &value,
        )?;
    }
    Ok(())
}

/// Copies `value` into a new coded side data entry of an output stream.
fn add_stream_side_data<T>(
    output: &mut ffmpeg::format::context::Output,
    stream: usize,
    kind: sys::AVPacketSideDataType,
    value: &T,
) -> Result<(), ffmpeg::Error> {
    let mut st = output
        .stream_mut(stream)
        .ok_or(ffmpeg::Error::StreamNotFound)?;
    // SAFETY: the StreamMut borrows the Output mutably, so its codecpar is valid and not
    // shared; av_packet_side_data_new allocates `size` bytes (or returns null) owned by
    // codecpar, and exactly that many bytes are copied from `value`, a plain C struct.
    unsafe {
        let par = &mut *(*st.as_mut_ptr()).codecpar;
        let size = size_of::<T>();
        let sd = sys::av_packet_side_data_new(
            &mut par.coded_side_data,
            &mut par.nb_coded_side_data,
            kind,
            size,
            0,
        );
        if sd.is_null() || (*sd).data.is_null() {
            return Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::ENOMEM,
            });
        }
        std::ptr::copy_nonoverlapping((value as *const T).cast::<u8>(), (*sd).data, size);
    }
    Ok(())
}

/// The display matrix of an input stream, if it has one.
pub(crate) fn display_matrix(stream: &ffmpeg::format::stream::Stream) -> Option<[i32; 9]> {
    stream_params(stream).display_matrix
}

/// Sets the codec tag (fourcc) of an output stream before the header is written, for
/// example `hvc1` so QuickTime players accept HEVC in MOV and MP4.
pub(crate) fn set_stream_codec_tag(
    output: &mut ffmpeg::format::context::Output,
    stream: usize,
    fourcc: [u8; 4],
) -> Result<(), ffmpeg::Error> {
    let mut st = output
        .stream_mut(stream)
        .ok_or(ffmpeg::Error::StreamNotFound)?;
    // SAFETY: the StreamMut borrows the Output mutably, so its codecpar is valid and not
    // shared; `codec_tag` is a plain integer the muxer reads when the header is written.
    unsafe {
        (*(*st.as_mut_ptr()).codecpar).codec_tag = u32::from_le_bytes(fourcc);
    }
    Ok(())
}

/// One plane of caller memory for [`Sws::scale`]: the bytes and the distance between
/// rows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SrcPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// A swscale context that converts pictures in caller memory (the render readback's
/// RGBA rows, YUV planes) into frames for an encoder, same size, another pixel format.
///
/// RGB to YUV uses the destination matrix, range and chroma position of `dst_yuv`; YUV to
/// YUV and RGB to RGB copy sample values (no matrix or range change).
pub(crate) struct Sws {
    ctx: std::ptr::NonNull<sys::SwsContext>,
    src: ffmpeg::format::Pixel,
    dst: ffmpeg::format::Pixel,
    width: u32,
    height: u32,
}

// SAFETY: the context is owned by this handle alone and only used through `&mut self`;
// swscale keeps no thread-affine state in it.
unsafe impl Send for Sws {}

impl Drop for Sws {
    fn drop(&mut self) {
        // SAFETY: the context this handle owns, freed once.
        unsafe { sys::sws_freeContext(self.ctx.as_ptr()) };
    }
}

impl std::fmt::Debug for Sws {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sws")
            .field("src", &self.src)
            .field("dst", &self.dst)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Sws {
    pub(crate) fn new(
        src: ffmpeg::format::Pixel,
        dst: ffmpeg::format::Pixel,
        width: u32,
        height: u32,
        dst_yuv: Option<SwsYuv>,
    ) -> Result<Self, ffmpeg::Error> {
        let w = c_int::try_from(width).map_err(|_| ffmpeg::Error::InvalidData)?;
        let h = c_int::try_from(height).map_err(|_| ffmpeg::Error::InvalidData)?;
        if w == 0 || h == 0 {
            return Err(ffmpeg::Error::InvalidData);
        }
        let src_rgb = pix_fmt_info(src).is_some_and(|i| i.rgb);
        let dst_rgb = pix_fmt_info(dst).is_some_and(|i| i.rgb);
        let flags = sys::SwsFlags::SWS_BICUBIC as c_uint
            | sys::SwsFlags::SWS_ACCURATE_RND as c_uint
            | sys::SwsFlags::SWS_FULL_CHR_H_INP as c_uint;
        // SAFETY: the context is allocated, configured through its public fields before
        // sws_init_context (as the API documents) and freed on the error paths; on
        // success the handle owns it.
        unsafe {
            let ctx = sys::sws_alloc_context();
            let Some(ctx) = std::ptr::NonNull::new(ctx) else {
                return Err(ffmpeg::Error::Other {
                    errno: ffmpeg::error::ENOMEM,
                });
            };
            let raw = ctx.as_ptr();
            (*raw).src_w = w;
            (*raw).src_h = h;
            (*raw).dst_w = w;
            (*raw).dst_h = h;
            (*raw).src_format = sys::AVPixelFormat::from(src) as c_int;
            (*raw).dst_format = sys::AVPixelFormat::from(dst) as c_int;
            (*raw).flags = flags;
            let yuv = dst_yuv.filter(|_| src_rgb && !dst_rgb);
            if let Some(yuv) = yuv {
                (*raw).src_range = 1;
                (*raw).dst_range = c_int::from(yuv.full_range);
                if let Some(p) = yuv.h_chroma_pos {
                    (*raw).dst_h_chr_pos = p;
                }
                if let Some(p) = yuv.v_chroma_pos {
                    (*raw).dst_v_chr_pos = p;
                }
            }
            let ret = sys::sws_init_context(raw, std::ptr::null_mut(), std::ptr::null_mut());
            if ret < 0 {
                sys::sws_freeContext(raw);
                return Err(ffmpeg::Error::from(ret));
            }
            if let Some(yuv) = yuv {
                let space: sys::AVColorSpace = yuv.matrix.into();
                let table = sys::sws_getCoefficients(space as c_int);
                let ret = sys::sws_setColorspaceDetails(
                    raw,
                    table,
                    1,
                    table,
                    c_int::from(yuv.full_range),
                    0,
                    1 << 16,
                    1 << 16,
                );
                if ret < 0 {
                    sys::sws_freeContext(raw);
                    return Err(ffmpeg::Error::from(ret));
                }
            }
            Ok(Self {
                ctx,
                src,
                dst,
                width,
                height,
            })
        }
    }

    /// Converts one picture in the source format, its planes in `planes`, into `out` (a
    /// frame of the destination format and size). Fails without touching memory when a
    /// plane is missing, a stride is shorter than a row or a plane holds fewer than
    /// `stride * rows` bytes.
    pub(crate) fn scale(
        &mut self,
        planes: &[SrcPlane<'_>],
        out: &mut ffmpeg::frame::Video,
    ) -> Result<(), ffmpeg::Error> {
        if out.format() != self.dst || out.width() != self.width || out.height() != self.height {
            return Err(ffmpeg::Error::InvalidData);
        }
        let count = pix_fmt_info(self.src).map_or(0, |i| i.planes);
        if count == 0 || count > 4 || planes.len() < count {
            return Err(ffmpeg::Error::InvalidData);
        }
        let src_id: sys::AVPixelFormat = self.src.into();
        let w = c_int::try_from(self.width).map_err(|_| ffmpeg::Error::InvalidData)?;
        let h = c_int::try_from(self.height).map_err(|_| ffmpeg::Error::InvalidData)?;
        let mut min_rows = [0 as c_int; 4];
        let mut strides = [0isize; 4];
        let mut sizes = [0usize; 4];
        let mut src_strides = [0 as c_int; 4];
        let mut src_ptrs: [*const u8; 4] = [std::ptr::null(); 4];
        for (i, plane) in planes.iter().take(count).enumerate() {
            strides[i] = isize::try_from(plane.stride).map_err(|_| ffmpeg::Error::InvalidData)?;
            src_strides[i] =
                c_int::try_from(plane.stride).map_err(|_| ffmpeg::Error::InvalidData)?;
            src_ptrs[i] = plane.data.as_ptr();
        }
        // SAFETY: both calls write at most four entries into the arrays passed, read the
        // static pixel format table and the four strides, and touch no picture memory.
        let ok = unsafe {
            sys::av_image_fill_linesizes(min_rows.as_mut_ptr(), src_id, w) >= 0
                && sys::av_image_fill_plane_sizes(sizes.as_mut_ptr(), src_id, h, strides.as_ptr())
                    >= 0
        };
        if !ok {
            return Err(ffmpeg::Error::InvalidData);
        }
        for (i, plane) in planes.iter().take(count).enumerate() {
            if src_strides[i] < min_rows[i] || plane.data.len() < sizes[i] {
                return Err(ffmpeg::Error::InvalidData);
            }
        }
        // SAFETY: every source plane was checked above to hold `stride * rows` bytes with
        // rows at least as long as the format needs at this width, so swscale reads only
        // inside the borrowed slices; the destination is a frame FFmpeg allocated at the
        // context's output format and size, borrowed mutably for the call.
        let written = unsafe {
            let dst = &mut *out.as_mut_ptr();
            sys::sws_scale(
                self.ctx.as_ptr(),
                src_ptrs.as_ptr(),
                src_strides.as_ptr(),
                0,
                h,
                dst.data.as_ptr(),
                dst.linesize.as_ptr(),
            )
        };
        if written != h {
            return Err(ffmpeg::Error::from(written.min(-1)));
        }
        Ok(())
    }
}

// ---- Hardware decode ----------------------------------------------------------------------

/// What a hardware decoder's `get_format` callback saw, shared with the callback through
/// `AVCodecContext.opaque`. FFmpeg may call the callback from its frame threads.
#[derive(Debug, Default)]
pub(crate) struct HwNegotiation {
    /// The decoder offered the hardware format at least once.
    pub offered: std::sync::atomic::AtomicBool,
}

/// A hardware device context (`AVHWDeviceContext`), reference counted by FFmpeg.
pub(crate) struct HwDevice(std::ptr::NonNull<sys::AVBufferRef>);

// SAFETY: the device context is created once, never mutated afterwards, and its reference
// count is atomic, so the handle can move between threads.
unsafe impl Send for HwDevice {}

impl Drop for HwDevice {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: `raw` is the reference this handle owns; av_buffer_unref releases it.
        unsafe { sys::av_buffer_unref(&mut raw) };
    }
}

impl HwDevice {
    /// Creates a VideoToolbox device context.
    #[cfg(target_os = "macos")]
    pub(crate) fn videotoolbox() -> Result<Self, ffmpeg::Error> {
        let mut ctx: *mut sys::AVBufferRef = std::ptr::null_mut();
        // SAFETY: plain out-pointer call; no device name or options.
        let ret = unsafe {
            sys::av_hwdevice_ctx_create(
                &mut ctx,
                sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            return Err(ffmpeg::Error::from(ret));
        }
        std::ptr::NonNull::new(ctx)
            .map(Self)
            .ok_or(ffmpeg::Error::Bug)
    }
}

/// Attaches `device` to a decoder context that is not open yet and installs a
/// `get_format` callback choosing VideoToolbox surfaces whenever the decoder offers them.
/// `negotiation` must outlive the codec context (the decoder owns both and drops the
/// context first).
#[cfg(target_os = "macos")]
pub(crate) fn attach_videotoolbox(
    ctx: &mut ffmpeg::codec::Context,
    device: &HwDevice,
    negotiation: &std::sync::Arc<HwNegotiation>,
) -> Result<(), ffmpeg::Error> {
    // SAFETY: the context is not open yet, so setting these fields is allowed; the new
    // buffer reference is owned (and freed) by the codec context.
    unsafe {
        let raw = ctx.as_mut_ptr();
        let reference = sys::av_buffer_ref(device.0.as_ptr());
        if reference.is_null() {
            return Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::ENOMEM,
            });
        }
        (*raw).hw_device_ctx = reference;
        (*raw).opaque = std::sync::Arc::as_ptr(negotiation) as *mut c_void;
        (*raw).get_format = Some(pick_videotoolbox);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn pick_videotoolbox(
    ctx: *mut sys::AVCodecContext,
    formats: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
    use sys::AVPixelFormat::{AV_PIX_FMT_NONE, AV_PIX_FMT_VIDEOTOOLBOX};
    // SAFETY: FFmpeg passes a list terminated by AV_PIX_FMT_NONE, and `opaque` points to
    // the HwNegotiation that `attach_videotoolbox` stored, which outlives the context.
    unsafe {
        let mut software = AV_PIX_FMT_NONE;
        let mut at = formats;
        while *at != AV_PIX_FMT_NONE {
            if *at == AV_PIX_FMT_VIDEOTOOLBOX {
                if let Some(negotiation) = ((*ctx).opaque as *const HwNegotiation).as_ref() {
                    negotiation
                        .offered
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                return AV_PIX_FMT_VIDEOTOOLBOX;
            }
            let hardware = sys::av_pix_fmt_desc_get(*at)
                .as_ref()
                .is_some_and(|d| d.flags & sys::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0);
            if !hardware && software == AV_PIX_FMT_NONE {
                software = *at;
            }
            at = at.add(1);
        }
        // FFmpeg retries without VideoToolbox when its session could not start; the
        // software format is then the one to take.
        software
    }
}

/// A retained `CVPixelBuffer` from a VideoToolbox frame.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub(crate) struct PixelBuffer(objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>);

// SAFETY: CoreVideo buffers are reference counted with atomic retain and release, and a
// decoded buffer is only read after the decoder hands it out, so sharing it between
// threads is sound.
#[cfg(target_os = "macos")]
unsafe impl Send for PixelBuffer {}
// SAFETY: see above; every method only reads immutable properties.
#[cfg(target_os = "macos")]
unsafe impl Sync for PixelBuffer {}

#[cfg(target_os = "macos")]
impl PixelBuffer {
    /// The pixel buffer behind a decoded VideoToolbox frame, retained, or `None` for any
    /// other frame.
    pub(crate) fn from_frame(frame: &ffmpeg::frame::Video) -> Option<Self> {
        // SAFETY: the frame wrapper owns a valid AVFrame; for AV_PIX_FMT_VIDEOTOOLBOX
        // frames FFmpeg stores the CVPixelBufferRef in data[3] and keeps it alive while
        // the frame lives, so retaining it here is sound.
        unsafe {
            let raw = frame.as_ptr();
            if (*raw).format != sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as c_int {
                return None;
            }
            let buffer = (*raw).data[3] as *mut objc2_core_video::CVPixelBuffer;
            std::ptr::NonNull::new(buffer)
                .map(|b| Self(objc2_core_foundation::CFRetained::retain(b)))
        }
    }

    pub(crate) fn buffer(&self) -> &objc2_core_video::CVPixelBuffer {
        &self.0
    }

    /// The CoreVideo pixel format as four characters (`420v`, `x420`).
    pub(crate) fn fourcc(&self) -> [u8; 4] {
        objc2_core_video::CVPixelBufferGetPixelFormatType(&self.0).to_be_bytes()
    }

    pub(crate) fn plane_count(&self) -> usize {
        objc2_core_video::CVPixelBufferGetPlaneCount(&self.0)
    }

    pub(crate) fn plane_size(&self, plane: usize) -> (u32, u32) {
        (
            objc2_core_video::CVPixelBufferGetWidthOfPlane(&self.0, plane) as u32,
            objc2_core_video::CVPixelBufferGetHeightOfPlane(&self.0, plane) as u32,
        )
    }

    pub(crate) fn io_surface(
        &self,
    ) -> Option<objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>> {
        objc2_core_video::CVPixelBufferGetIOSurface(Some(&self.0))
    }

    /// BLAKE3 of the first `row_bytes(plane)` bytes of every row of every plane, read
    /// through a read-only CPU lock of the surface. For tests that compare decoded
    /// pictures; playback never reads surfaces on the CPU.
    pub(crate) fn content_hash(&self, row_bytes: impl Fn(usize) -> usize) -> Option<[u8; 32]> {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let flags = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: locking a retained buffer read-only; unlocked below on every path.
        if unsafe { CVPixelBufferLockBaseAddress(&self.0, flags) } != 0 {
            return None;
        }
        let mut hasher = blake3::Hasher::new();
        let mut ok = true;
        for plane in 0..self.plane_count() {
            let base = CVPixelBufferGetBaseAddressOfPlane(&self.0, plane) as *const u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&self.0, plane);
            let (_, height) = self.plane_size(plane);
            let len = row_bytes(plane);
            if base.is_null() || len > stride {
                ok = false;
                break;
            }
            for row in 0..height as usize {
                // SAFETY: the base address is locked and the plane holds `height` rows of
                // `stride` bytes; `len <= stride` was checked.
                let bytes = unsafe { std::slice::from_raw_parts(base.add(row * stride), len) };
                hasher.update(bytes);
            }
        }
        // SAFETY: balances the lock above.
        unsafe { CVPixelBufferUnlockBaseAddress(&self.0, flags) };
        ok.then(|| *hasher.finalize().as_bytes())
    }
}

/// CoreMedia codec types (`kCMVideoCodecType_*`) are big-endian four-character codes.
#[cfg(target_os = "macos")]
pub(crate) fn videotoolbox_hardware_decode_supported(codec_type: [u8; 4]) -> bool {
    #[link(name = "VideoToolbox", kind = "framework")]
    unsafe extern "C" {
        fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
    }
    // SAFETY: a pure query taking a codec type by value (macOS 10.13 and later).
    unsafe { VTIsHardwareDecodeSupported(u32::from_be_bytes(codec_type)) != 0 }
}
