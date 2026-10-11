//! Text subtitle streams: [`SubtitleCue`]s encoded into a container's subtitle stream by
//! the export session (see [`crate::export`]) and read back from one
//! ([`read_subtitle_cues`]).
//!
//! FFmpeg's text subtitle encoders take each cue as an ASS dialogue line: lines break
//! at `\N`, `{\` opens an override block and a backslash before `n`, `N` or `h` is an
//! escape. A cue's backslashes and the `{` of a `{\` pair are therefore followed by a
//! word joiner (U+2060), as FFmpeg's own decoders do, so they reach the file as text;
//! reading removes the joiners again. WebVTT payloads escape `&`, `<` and `>` (the
//! `webvtt` encoder writes text as it is).

use std::path::Path;

use ffmpeg_next as ffmpeg;

use crate::demux::{Demuxer, Packet};
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;
use crate::time::Rational;

/// One subtitle cue in whole milliseconds from the start of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleCue {
    pub start_ms: i64,
    /// After `start_ms`.
    pub end_ms: i64,
    /// Lines separated by `\n`.
    pub text: String,
}

/// The FFmpeg subtitle encoders the export session uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubtitleCodec {
    /// 3GPP timed text (`tx3g`), the subtitle stream MP4 and QuickTime players read.
    MovText,
    /// WebVTT, Matroska's `D_WEBVTT/SUBTITLES` and the WebM subtitle codec.
    WebVtt,
}

impl SubtitleCodec {
    pub(crate) fn encoder(self) -> &'static str {
        match self {
            Self::MovText => "mov_text",
            Self::WebVtt => "webvtt",
        }
    }
}

/// The milliseconds every subtitle packet is timed in.
const TIME_BASE: Rational = Rational::new(1, 1000);

/// The script header the encoders read their default style from (FFmpeg's default:
/// Arial 16 at 384 x 288, white, bottom centre).
const ASS_HEADER: &str = "[Script Info]\r\n\
ScriptType: v4.00+\r\n\
PlayResX: 384\r\n\
PlayResY: 288\r\n\
ScaledBorderAndShadow: yes\r\n\
\r\n\
[V4+ Styles]\r\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, \
Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, \
Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\r\n\
Style: Default,Arial,16,&Hffffff,&Hffffff,&H0,&H0,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\r\n\
\r\n\
[Events]\r\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\r\n";

/// An open text subtitle encoder: cues in, one packet per cue out.
pub(crate) struct SubtitleEncoder {
    inner: ffmpeg::encoder::subtitle::Encoder,
    codec: SubtitleCodec,
    buffer: Vec<u8>,
    read_order: u64,
}

impl SubtitleEncoder {
    pub(crate) fn open(codec: SubtitleCodec, global_header: bool) -> Result<Self> {
        crate::init();
        let name = codec.encoder();
        let error = |action: &'static str| {
            move |e: ffmpeg::Error| MediaError::Encode {
                encoder: name.to_string(),
                action,
                source: e.into(),
            }
        };
        let found = ffmpeg::encoder::find_by_name(name)
            .ok_or(ffmpeg::Error::EncoderNotFound)
            .map_err(error("finding the encoder"))?;
        let mut context = ffmpeg::codec::Context::new_with_codec(found);
        if global_header {
            context.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        ffi::set_subtitle_header(&mut context, ASS_HEADER)
            .map_err(error("setting the subtitle header"))?;
        let mut encoder = context
            .encoder()
            .subtitle()
            .map_err(error("configuring the encoder"))?;
        encoder.set_time_base(ffmpeg::Rational::from(TIME_BASE));
        let inner = encoder
            .open_as(found)
            .map_err(error("opening the encoder"))?;
        Ok(Self {
            inner,
            codec,
            buffer: vec![0; 64 * 1024],
            read_order: 0,
        })
    }

    pub(crate) fn name(&self) -> &'static str {
        self.codec.encoder()
    }

    pub(crate) fn time_base(&self) -> Rational {
        TIME_BASE
    }

    pub(crate) fn codec(&self) -> Option<ffmpeg::Codec> {
        self.inner.codec()
    }

    pub(crate) fn parameters(&self) -> ffmpeg::codec::Parameters {
        ffmpeg::codec::Parameters::from(&self.inner)
    }

    /// The cue as one packet timed in milliseconds.
    pub(crate) fn encode(&mut self, cue: &SubtitleCue) -> Result<Packet> {
        let text = match self.codec {
            SubtitleCodec::MovText => cue.text.clone(),
            SubtitleCodec::WebVtt => vtt_escape(&cue.text),
        };
        let dialogue = format!("{},0,Default,,0,0,0,,{}", self.read_order, ass_text(&text));
        self.read_order += 1;
        let duration = cue.end_ms - cue.start_ms;
        let display = u32::try_from(duration).unwrap_or(u32::MAX);
        let size = ffi::encode_subtitle(&mut self.inner, &dialogue, display, &mut self.buffer)
            .map_err(|e| MediaError::Encode {
                encoder: self.name().to_string(),
                action: "encoding a cue",
                source: e.into(),
            })?;
        let mut packet = ffmpeg::Packet::copy(&self.buffer[..size]);
        packet.set_pts(Some(cue.start_ms));
        packet.set_dts(Some(cue.start_ms));
        packet.set_duration(duration);
        packet.set_flags(ffmpeg::packet::Flags::KEY);
        Ok(Packet::from_encoder(packet, TIME_BASE))
    }
}

/// Every cue of subtitle `stream` of the file at `path`, decoded with FFmpeg, in file
/// order. Empty samples (the gaps MP4 stores between cues) are skipped.
pub fn read_subtitle_cues(path: impl AsRef<Path>, stream: usize) -> Result<Vec<SubtitleCue>> {
    let mut demuxer = Demuxer::open(path)?;
    let summary = demuxer.stream(stream)?.clone();
    let path = demuxer.path().to_path_buf();
    if summary.kind != MediaKind::Subtitle {
        return Err(MediaError::NotDecodable {
            path,
            index: stream,
            kind: summary.kind,
        });
    }
    let failed = |action: &'static str| {
        let path = path.clone();
        move |e: ffmpeg::Error| MediaError::Codec {
            path,
            index: stream,
            action,
            source: e.into(),
        }
    };
    let parameters = demuxer
        .input()
        .stream(stream)
        .map(|s| s.parameters())
        .ok_or_else(|| MediaError::NoSuchStream {
            path: path.clone(),
            index: stream,
        })?;
    let mut decoder = ffmpeg::codec::Context::from_parameters(parameters)
        .map_err(failed("opening the decoder"))?
        .decoder()
        .subtitle()
        .map_err(failed("opening the decoder"))?;
    let ms = |value: i64| -> i64 {
        let tb = summary.time_base;
        let num = i128::from(value) * i128::from(tb.num) * 1000;
        let den = i128::from(tb.den.max(1));
        ((2 * num + den).div_euclid(2 * den)) as i64
    };
    let mut cues = Vec::new();
    while let Some(packet) = demuxer.read_packet()? {
        if packet.stream_index() != stream {
            continue;
        }
        let Some(texts) = ffi::decode_subtitle_texts(&mut decoder, packet.raw())
            .map_err(failed("decoding a cue"))?
        else {
            continue;
        };
        let text = texts
            .iter()
            .map(|ass| ass_dialogue_text(ass))
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            continue;
        }
        let start = packet.raw().pts().or(packet.raw().dts()).unwrap_or(0);
        let duration = packet.raw().duration();
        cues.push(SubtitleCue {
            start_ms: ms(start),
            end_ms: ms(start + duration),
            text,
        });
    }
    Ok(cues)
}

/// `&`, `<` and `>` as character references.
fn vtt_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A cue's text as the text field of an ASS dialogue line (see the module docs).
fn ass_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => out.push_str("\\N"),
            '\\' => out.push_str("\\\u{2060}"),
            '{' if chars.peek() == Some(&'\\') => out.push_str("{\u{2060}"),
            '\0' => {}
            c => out.push(c),
        }
    }
    out
}

/// The plain text of a decoded ASS dialogue line (`ReadOrder, Layer, Style, Name,
/// MarginL, MarginR, MarginV, Effect, Text`): override blocks dropped, `\N` and `\n` as
/// line breaks, `\h` as a space, escaped braces and the word joiners after backslashes
/// and braces removed.
fn ass_dialogue_text(dialogue: &str) -> String {
    let text = dialogue.splitn(9, ',').nth(8).unwrap_or(dialogue);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some('N' | 'n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('h') => {
                    chars.next();
                    out.push(' ');
                }
                Some(&b @ ('{' | '}')) => {
                    chars.next();
                    out.push(b);
                }
                _ => out.push('\\'),
            },
            '{' if matches!(chars.peek(), Some('\\' | '}')) => {
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                }
            }
            // Joiners after a backslash or brace (one from the writer, one more from
            // FFmpeg's WebVTT decoder).
            '\u{2060}' if out.ends_with(['\\', '{']) => {}
            c => out.push(c),
        }
    }
    out.trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_text_round_trips_markup_characters() {
        for text in [
            "Plain",
            "Two\nlines",
            "Back\\slash \\N not a break",
            "{\\i1} not a tag {braces}",
            "Fish & chips <b>",
        ] {
            let dialogue = format!("0,0,Default,,0,0,0,,{}", ass_text(text));
            assert_eq!(ass_dialogue_text(&dialogue), text, "{dialogue:?}");
        }
        assert_eq!(ass_text("a\r\nb"), "a\\Nb");
    }

    #[test]
    fn decoded_override_blocks_and_escapes_are_dropped() {
        assert_eq!(
            ass_dialogue_text("3,0,Default,,0,0,0,,{\\i1}Hi{\\i0},\\Nthere\\h!"),
            "Hi,\nthere !"
        );
        // FFmpeg's WebVTT decoder escapes braces as `\{{}`.
        assert_eq!(ass_dialogue_text("0,0,Default,,0,0,0,,a\\{{}b"), "a{b");
    }
}
