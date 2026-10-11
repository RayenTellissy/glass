//! Animated GIF output through the `gif` crate: each frame gets its own 256-colour
//! palette (NeuQuant), fully transparent pixels stay transparent, and the animation loops.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::time::Rational;

use super::{ExportError, Result};

/// NeuQuant sampling: 1 is best and slowest, 30 fastest; 10 is the crate's own advice.
const QUANTISER_SPEED: i32 = 10;

pub(crate) struct GifWriter {
    encoder: gif::Encoder<BufWriter<File>>,
    path: PathBuf,
    width: u16,
    height: u16,
    frame_rate: Rational,
    frames: u64,
    rgba: Vec<u8>,
}

impl std::fmt::Debug for GifWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GifWriter")
            .field("path", &self.path)
            .field("frames", &self.frames)
            .finish()
    }
}

fn write_error(path: &Path, error: impl std::fmt::Display) -> ExportError {
    ExportError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

impl GifWriter {
    pub(crate) fn create(
        path: &Path,
        width: u32,
        height: u32,
        frame_rate: Rational,
    ) -> Result<Self> {
        let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(ExportError::Settings(format!("a {width}x{height} GIF")));
        };
        let file = File::create(path).map_err(|e| write_error(path, e))?;
        let mut encoder =
            gif::Encoder::new(BufWriter::new(file), w, h, &[]).map_err(|e| write_error(path, e))?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .map_err(|e| write_error(path, e))?;
        Ok(Self {
            encoder,
            path: path.to_path_buf(),
            width: w,
            height: h,
            frame_rate,
            frames: 0,
            rgba: Vec::new(),
        })
    }

    /// Hundredths of a second from the start to frame `n`, rounded, so delays alternate
    /// (3, 3, 4 at 30 fps) and never drift.
    fn centiseconds(&self, n: u64) -> u64 {
        let num = u64::try_from(self.frame_rate.num).unwrap_or(1).max(1);
        let den = u64::try_from(self.frame_rate.den).unwrap_or(1);
        (n * 100 * den * 2 + num) / (num * 2)
    }

    /// Writes one RGBA frame (FFmpeg `rgba`, rows possibly padded).
    pub(crate) fn write(&mut self, frame: &ffmpeg::frame::Video) -> Result<()> {
        let row = usize::from(self.width) * 4;
        let stride = frame.stride(0);
        let data = frame.data(0);
        self.rgba.clear();
        for y in 0..usize::from(self.height) {
            self.rgba
                .extend_from_slice(&data[y * stride..y * stride + row]);
        }
        let mut out =
            gif::Frame::from_rgba_speed(self.width, self.height, &mut self.rgba, QUANTISER_SPEED);
        let delay = self.centiseconds(self.frames + 1) - self.centiseconds(self.frames);
        out.delay = u16::try_from(delay).unwrap_or(u16::MAX);
        out.dispose = gif::DisposalMethod::Background;
        self.encoder
            .write_frame(&out)
            .map_err(|e| write_error(&self.path, e))?;
        self.frames += 1;
        Ok(())
    }

    /// Writes the trailer and flushes the file.
    pub(crate) fn finish(self) -> Result<()> {
        let path = self.path;
        let writer = self
            .encoder
            .into_inner()
            .map_err(|e| write_error(&path, e))?;
        let file = writer
            .into_inner()
            .map_err(|e| write_error(&path, e.error()))?;
        file.sync_all().map_err(|e| write_error(&path, e))
    }
}
