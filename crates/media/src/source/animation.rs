//! Animated GIF and WebP. Frame delays make a variable-rate [`SourceTimeline`] in
//! milliseconds, read from the file's frame headers without decoding pixels; frames are
//! decoded on demand and composited onto the canvas (GIF disposal, WebP blending), so a
//! seek forward decodes up to the target and a seek backward starts over from the first
//! frame (every frame can depend on the ones before it).
//!
//! The `gif` and `image-webp` decoders are used directly rather than through `image`'s
//! frame iterator, which is not `Send` and so could not stay on a decode-pool thread
//! between calls.
//!
//! Delays follow what browsers do: a delay of 10 ms or less (including 0) plays as 100 ms.

use std::io::BufReader;
use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbImage, RgbaImage};

use super::icc::EmbeddedColour;
use super::still::{ImageKind, frame_from_image, io_error};
use crate::decode::VideoFrame;
use crate::error::{MediaError, Result};
use crate::time::{Rational, Timestamp};
use crate::timemap::{FrameTimes, SourceTimeline};

/// Milliseconds: animation timestamps.
pub(crate) const MILLIS: Rational = Rational::new(1, 1000);

/// The shortest delay that plays as written; shorter ones play as [`DEFAULT_DELAY_MS`].
const MIN_DELAY_MS: u32 = 11;
const DEFAULT_DELAY_MS: u32 = 100;

fn effective_delay(ms: u32) -> u32 {
    if ms < MIN_DELAY_MS {
        DEFAULT_DELAY_MS
    } else {
        ms
    }
}

/// What an animated file says about its frames, read without decoding them.
#[derive(Clone, Debug)]
pub(crate) struct AnimationHeader {
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    /// Delay of each frame as stored, in milliseconds.
    pub delays_ms: Vec<u32>,
    pub colour: EmbeddedColour,
}

impl AnimationHeader {
    pub fn timeline(&self) -> SourceTimeline {
        let mut pts = Vec::with_capacity(self.delays_ms.len());
        let mut t = 0i64;
        for d in &self.delays_ms {
            pts.push(t);
            t += i64::from(effective_delay(*d));
        }
        SourceTimeline {
            stream_index: 0,
            time_base: MILLIS,
            frames: FrameTimes::Variable { pts, end: t },
        }
    }
}

pub(crate) fn read_header(path: &Path, kind: ImageKind) -> Result<AnimationHeader> {
    match kind {
        ImageKind::Gif => gif_header(path),
        ImageKind::WebP => webp_header(path),
        _ => Err(bad(path, "not an animation format")),
    }
}

fn bad(path: &Path, message: impl std::fmt::Display) -> MediaError {
    MediaError::Image {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

fn gif_header(path: &Path) -> Result<AnimationHeader> {
    let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut options = gif::DecodeOptions::new();
    options.skip_frame_decoding(true);
    let mut decoder = options
        .read_info(BufReader::new(file))
        .map_err(|e| bad(path, e))?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    let mut delays_ms = Vec::new();
    let mut alpha = false;
    while let Some(frame) = decoder.read_next_frame().map_err(|e| bad(path, e))? {
        delays_ms.push(u32::from(frame.delay) * 10);
        alpha |= frame.transparent.is_some();
    }
    if delays_ms.is_empty() {
        return Err(bad(path, "the GIF has no frames"));
    }
    Ok(AnimationHeader {
        kind: ImageKind::Gif,
        width,
        height,
        alpha,
        delays_ms,
        colour: EmbeddedColour::default(),
    })
}

/// Walks a WebP file's RIFF chunks: `VP8X` for the canvas and flags, `ANMF` for each
/// frame's duration, `ICCP` for the profile.
fn webp_header(path: &Path) -> Result<AnimationHeader> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, &e))?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(bad(path, "not a WebP file"));
    }
    let u24 = |d: &[u8]| u32::from(d[0]) | u32::from(d[1]) << 8 | u32::from(d[2]) << 16;
    let mut at = 12;
    let (mut width, mut height, mut alpha) = (0, 0, false);
    let mut delays_ms = Vec::new();
    let mut colour = EmbeddedColour::default();
    while at + 8 <= bytes.len() {
        let kind = &bytes[at..at + 4];
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let data = bytes
            .get(at + 8..at + 8 + len)
            .ok_or_else(|| bad(path, "truncated WebP chunk"))?;
        match kind {
            b"VP8X" if len >= 10 => {
                alpha = data[0] & 0x10 != 0;
                width = u24(&data[4..7]) + 1;
                height = u24(&data[7..10]) + 1;
            }
            b"ANMF" if len >= 16 => delays_ms.push(u24(&data[12..15])),
            b"ICCP" => {
                colour = super::icc::parse_icc(data).unwrap_or_default();
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    if delays_ms.is_empty() {
        return Err(bad(path, "the WebP file is not animated"));
    }
    Ok(AnimationHeader {
        kind: ImageKind::WebP,
        width,
        height,
        alpha,
        delays_ms,
        colour,
    })
}

enum Decoder {
    Gif {
        decoder: Box<gif::Decoder<BufReader<std::fs::File>>>,
        canvas: Vec<u8>,
        /// The canvas before the last frame, for `DisposalMethod::Previous`.
        previous: Option<Vec<u8>>,
        /// How to dispose of the last frame drawn before drawing the next one.
        pending_dispose: Option<(gif::DisposalMethod, [u32; 4])>,
    },
    WebP {
        decoder: Box<image_webp::WebPDecoder<BufReader<std::fs::File>>>,
    },
}

/// Decodes an animation's frames in order, as planar RGB frames with millisecond pts.
pub(crate) struct AnimationReader {
    path: PathBuf,
    header: AnimationHeader,
    timeline: SourceTimeline,
    decoder: Option<Decoder>,
    /// Index of the next frame the decoder will produce.
    next: usize,
}

impl std::fmt::Debug for AnimationReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimationReader")
            .field("path", &self.path)
            .field("frames", &self.header.delays_ms.len())
            .field("next", &self.next)
            .finish()
    }
}

impl AnimationReader {
    pub fn new(path: &Path, header: AnimationHeader) -> Self {
        let timeline = header.timeline();
        Self {
            path: path.to_path_buf(),
            header,
            timeline,
            decoder: None,
            next: 0,
        }
    }

    pub fn timeline(&self) -> &SourceTimeline {
        &self.timeline
    }

    /// Starts decoding from the first frame again.
    pub fn rewind(&mut self) {
        self.decoder = None;
        self.next = 0;
    }

    fn open_decoder(&self) -> Result<Decoder> {
        let path = &self.path;
        let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
        let reader = BufReader::new(file);
        Ok(match self.header.kind {
            ImageKind::Gif => {
                let mut options = gif::DecodeOptions::new();
                options.set_color_output(gif::ColorOutput::RGBA);
                let decoder = options.read_info(reader).map_err(|e| bad(path, e))?;
                let (w, h) = (self.header.width as usize, self.header.height as usize);
                Decoder::Gif {
                    decoder: Box::new(decoder),
                    canvas: vec![0; w * h * 4],
                    previous: None,
                    pending_dispose: None,
                }
            }
            _ => {
                let mut decoder = image_webp::WebPDecoder::new(reader).map_err(|e| bad(path, e))?;
                if !decoder.is_animated() {
                    return Err(bad(path, "the WebP file is not animated"));
                }
                // Browsers show uncovered canvas as transparent, not the file's hint.
                if decoder.has_alpha() {
                    let _ = decoder.set_background_color([0, 0, 0, 0]);
                }
                Decoder::WebP {
                    decoder: Box::new(decoder),
                }
            }
        })
    }

    /// Decodes the next frame, or `None` after the last one.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        if self.next >= self.header.delays_ms.len() {
            return Ok(None);
        }
        if self.decoder.is_none() {
            self.decoder = Some(self.open_decoder()?);
        }
        let image = self.decode_next()?;
        let Some(image) = image else {
            // Fewer frames than the headers listed: stop there.
            self.next = self.header.delays_ms.len();
            return Ok(None);
        };
        let index = self.next;
        self.next += 1;
        let pts = self
            .timeline
            .frame_pts(index as u64)
            .unwrap_or(Timestamp::new(0, MILLIS));
        let duration = i64::from(effective_delay(self.header.delays_ms[index]));
        Ok(Some(frame_from_image(
            &image,
            &self.header.colour,
            pts,
            Some(duration),
        )))
    }

    /// The frame with index `k`: decodes forward from the current position, or from the
    /// first frame when `k` is behind it.
    pub fn frame(&mut self, k: usize) -> Result<Option<VideoFrame>> {
        if k >= self.header.delays_ms.len() {
            return Ok(None);
        }
        if k < self.next {
            self.rewind();
        }
        while self.next < k {
            if self.skip_frame()?.is_none() {
                return Ok(None);
            }
        }
        self.next_frame()
    }

    fn skip_frame(&mut self) -> Result<Option<()>> {
        if self.decoder.is_none() {
            self.decoder = Some(self.open_decoder()?);
        }
        let decoded = self.decode_next()?;
        if decoded.is_some() {
            self.next += 1;
        }
        Ok(decoded.map(|_| ()))
    }

    fn decode_next(&mut self) -> Result<Option<DynamicImage>> {
        let (w, h) = (self.header.width, self.header.height);
        let alpha = self.header.alpha;
        let path = self.path.clone();
        match self.decoder.as_mut() {
            Some(Decoder::Gif {
                decoder,
                canvas,
                previous,
                pending_dispose,
            }) => {
                if let Some((method, rect)) = pending_dispose.take() {
                    match method {
                        gif::DisposalMethod::Background => clear_rect(canvas, w, rect),
                        gif::DisposalMethod::Previous => {
                            if let Some(before) = previous.take() {
                                *canvas = before;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(frame) = decoder.read_next_frame().map_err(|e| bad(&path, e))? else {
                    return Ok(None);
                };
                let rect = [
                    u32::from(frame.left),
                    u32::from(frame.top),
                    u32::from(frame.width),
                    u32::from(frame.height),
                ];
                if frame.dispose == gif::DisposalMethod::Previous {
                    *previous = Some(canvas.clone());
                }
                draw_gif_frame(canvas, w, h, rect, &frame.buffer);
                *pending_dispose = Some((frame.dispose, rect));
                let rgba = RgbaImage::from_raw(w, h, canvas.clone())
                    .ok_or_else(|| bad(&path, "GIF canvas size mismatch"))?;
                Ok(Some(if alpha {
                    DynamicImage::ImageRgba8(rgba)
                } else {
                    DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
                }))
            }
            Some(Decoder::WebP { decoder }) => {
                let Some(size) = decoder.output_buffer_size() else {
                    return Err(bad(&path, "WebP canvas too large"));
                };
                let mut buffer = vec![0u8; size];
                match decoder.read_frame(&mut buffer) {
                    Ok(_) => {}
                    Err(image_webp::DecodingError::NoMoreFrames) => return Ok(None),
                    Err(e) => return Err(bad(&path, e)),
                }
                let image = if decoder.has_alpha() {
                    RgbaImage::from_raw(w, h, buffer).map(DynamicImage::ImageRgba8)
                } else {
                    RgbImage::from_raw(w, h, buffer).map(DynamicImage::ImageRgb8)
                };
                image
                    .map(Some)
                    .ok_or_else(|| bad(&path, "WebP canvas size mismatch"))
            }
            None => Ok(None),
        }
    }
}

/// Draws a GIF frame's RGBA pixels onto the canvas; fully transparent pixels leave the
/// canvas as it was.
fn draw_gif_frame(canvas: &mut [u8], width: u32, height: u32, rect: [u32; 4], pixels: &[u8]) {
    let [left, top, w, h] = rect;
    for y in 0..h {
        let cy = top + y;
        if cy >= height {
            break;
        }
        for x in 0..w {
            let cx = left + x;
            if cx >= width {
                break;
            }
            let src = ((y * w + x) * 4) as usize;
            let Some(px) = pixels.get(src..src + 4) else {
                return;
            };
            if px[3] != 0 {
                let dst = ((cy * width + cx) * 4) as usize;
                canvas[dst..dst + 4].copy_from_slice(px);
            }
        }
    }
}

fn clear_rect(canvas: &mut [u8], width: u32, rect: [u32; 4]) {
    let [left, top, w, h] = rect;
    let height = canvas.len() as u32 / 4 / width.max(1);
    for y in top..(top + h).min(height) {
        for x in left..(left + w).min(width) {
            let dst = ((y * width + x) * 4) as usize;
            canvas[dst..dst + 4].fill(0);
        }
    }
}
