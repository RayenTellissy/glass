//! HEIC, HEIF and AVIF stills through macOS ImageIO: `CGImageSource` reads the file and
//! Core Graphics draws the primary image into a bitmap in the image's own RGB colour
//! space, so pixels keep their encoding and the ICC profile says what it is.
//!
//! Only the primary image's SDR rendition is read: HDR gain maps (auxiliary images) and
//! PQ or HLG renditions are ignored for now, and a picture deeper than 8 bits is read as
//! 16-bit samples.

use std::ffi::c_void;

use objc2_core_foundation::{CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGColorSpaceModel, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyDepth, kCGImagePropertyHasAlpha, kCGImagePropertyOrientation,
    kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
};

/// What ImageIO says about the primary image, without decoding it.
pub(crate) struct Info {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    /// More than 8 bits per component.
    pub deep: bool,
    /// EXIF orientation, 1 to 8.
    pub orientation: u8,
    /// The image's ICC profile (its colour space's, when it is RGB).
    pub icc: Option<Vec<u8>>,
}

pub(crate) enum Pixels {
    /// Interleaved RGBA, straight (not premultiplied) alpha.
    Rgba8(Vec<u8>),
    Rgba16(Vec<u16>),
}

/// The primary image's pixels, stored orientation, in its own colour space.
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub deep: bool,
    pub orientation: u8,
    pub icc: Option<Vec<u8>>,
    pub pixels: Pixels,
}

struct Source {
    source: CFRetained<CGImageSource>,
    index: usize,
}

fn open(bytes: &[u8]) -> Result<Source, String> {
    let data = CFData::from_bytes(bytes);
    // SAFETY: `data` is a valid CFData; no options dictionary.
    let source =
        unsafe { CGImageSource::with_data(&data, None) }.ok_or("ImageIO cannot read this file")?;
    // SAFETY: `source` is a valid image source for all calls below.
    let count = unsafe { source.count() };
    if count == 0 {
        return Err("ImageIO found no image in the file".to_string());
    }
    // SAFETY: as above.
    let index = unsafe { source.primary_image_index() }.min(count - 1);
    Ok(Source { source, index })
}

fn number(properties: &CFDictionary, key: &CFString) -> Option<i64> {
    // SAFETY: ImageIO property dictionaries map CFString keys to CFType values; the value
    // is only used after a checked downcast to CFNumber.
    let dict = unsafe { properties.cast_unchecked::<CFString, CFType>() };
    let value = dict.get(key)?;
    let number = value.downcast_ref::<CFNumber>()?;
    number.as_i64()
}

fn boolean(properties: &CFDictionary, key: &CFString) -> Option<bool> {
    number(properties, key).map(|v| v != 0)
}

impl Source {
    fn info(&self) -> Result<(Info, CFRetained<CGImage>), String> {
        // SAFETY: valid source and index.
        let properties = unsafe { self.source.properties_at_index(self.index, None) }
            .ok_or("ImageIO has no properties for the image")?;
        // SAFETY: the kCGImageProperty keys are static CFStrings exported by ImageIO.
        let (w_key, h_key, o_key, a_key, d_key) = unsafe {
            (
                kCGImagePropertyPixelWidth,
                kCGImagePropertyPixelHeight,
                kCGImagePropertyOrientation,
                kCGImagePropertyHasAlpha,
                kCGImagePropertyDepth,
            )
        };
        // SAFETY: valid source and index. Creating the image does not decode it yet.
        let image = unsafe { self.source.image_at_index(self.index, None) }
            .ok_or("ImageIO cannot decode the image")?;
        let width =
            number(&properties, w_key).unwrap_or_else(|| CGImage::width(Some(&image)) as i64);
        let height =
            number(&properties, h_key).unwrap_or_else(|| CGImage::height(Some(&image)) as i64);
        let orientation = number(&properties, o_key)
            .filter(|o| (1..=8).contains(o))
            .unwrap_or(1) as u8;
        let alpha = boolean(&properties, a_key).unwrap_or_else(|| {
            !matches!(
                CGImage::alpha_info(Some(&image)),
                CGImageAlphaInfo::None
                    | CGImageAlphaInfo::NoneSkipFirst
                    | CGImageAlphaInfo::NoneSkipLast
            )
        });
        let depth = number(&properties, d_key)
            .unwrap_or_else(|| CGImage::bits_per_component(Some(&image)) as i64);
        let icc = CGImage::color_space(Some(&image))
            .filter(|space| CGColorSpace::model(Some(space)) == CGColorSpaceModel::RGB)
            .and_then(|space| CGColorSpace::icc_data(Some(&space)))
            .map(|data| data.to_vec());
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err("ImageIO reported an invalid size".to_string());
        };
        Ok((
            Info {
                width,
                height,
                alpha,
                deep: depth > 8,
                orientation,
                icc,
            },
            image,
        ))
    }
}

pub(crate) fn probe(bytes: &[u8]) -> Result<Info, String> {
    open(bytes)?.info().map(|(info, _)| info)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Picture, String> {
    let (info, image) = open(bytes)?.info()?;
    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    // Draw in the image's own RGB space, so no colour conversion happens; anything else
    // (grey, CMYK) is drawn into sRGB.
    let space = CGImage::color_space(Some(&image))
        .filter(|space| CGColorSpace::model(Some(space)) == CGColorSpaceModel::RGB)
        .or_else(|| {
            // SAFETY: kCGColorSpaceSRGB is a static CFString exported by Core Graphics.
            CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB }))
        })
        .ok_or("no RGB colour space to draw the image into")?;
    let icc = if info.icc.is_some() {
        info.icc.clone()
    } else {
        CGColorSpace::icc_data(Some(&space)).map(|data| data.to_vec())
    };
    let alpha = if info.alpha {
        CGImageAlphaInfo::PremultipliedLast
    } else {
        CGImageAlphaInfo::NoneSkipLast
    };
    let rect = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(width as f64, height as f64),
    );
    let context_for = |data: *mut c_void, bits: usize, order: CGImageByteOrderInfo| {
        let row = width * 4 * bits / 8;
        // SAFETY: `data` points at `row * height` writable bytes that outlive the
        // context, which is dropped before the buffer is returned.
        unsafe {
            CGBitmapContextCreate(
                data,
                width,
                height,
                bits,
                row,
                Some(&space),
                alpha.0 | order.0,
            )
        }
    };
    let draw = |context: &CGContext| CGContext::draw_image(Some(context), rect, Some(&image));

    let pixels = if info.deep {
        let mut buffer = vec![0u16; width * height * 4];
        let context = context_for(
            buffer.as_mut_ptr().cast(),
            16,
            CGImageByteOrderInfo::Order16Little,
        )
        .ok_or("Core Graphics cannot make a 16-bit bitmap for the image")?;
        draw(&context);
        drop(context);
        if info.alpha {
            for px in buffer.as_chunks_mut::<4>().0 {
                let a = u32::from(px[3]);
                if a > 0 && a < 65535 {
                    for c in &mut px[..3] {
                        *c = ((u32::from(*c) * 65535 + a / 2) / a).min(65535) as u16;
                    }
                }
            }
        }
        Pixels::Rgba16(buffer)
    } else {
        let mut buffer = vec![0u8; width * height * 4];
        let context = context_for(
            buffer.as_mut_ptr().cast(),
            8,
            CGImageByteOrderInfo::OrderDefault,
        )
        .ok_or("Core Graphics cannot make an 8-bit bitmap for the image")?;
        draw(&context);
        drop(context);
        if info.alpha {
            for px in buffer.as_chunks_mut::<4>().0 {
                let a = u32::from(px[3]);
                if a > 0 && a < 255 {
                    for c in &mut px[..3] {
                        *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        Pixels::Rgba8(buffer)
    };
    Ok(Picture {
        width: width as u32,
        height: height as u32,
        alpha: info.alpha,
        deep: info.deep,
        orientation: info.orientation,
        icc,
        pixels,
    })
}
