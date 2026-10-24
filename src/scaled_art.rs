//! Art decoded at the largest size it is drawn on this screen, so a 600x900 cover shown in a
//! 164 px tile doesn't keep 2 MB of pixels (plus its GPU copy) for the whole session, and a 4K
//! background costs no more than the screen needs. Art is never enlarged, and never made smaller
//! than it is drawn.

use std::path::Path;

use image::imageops::{self, FilterType};
use image::{DynamicImage, Rgba32FImage};
use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, IsProcessDPIAware, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
};

/// What the art is used as; each kind is drawn in its own boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Icon,
    Logo,
    /// The full-screen background.
    Hero,
}

impl Kind {
    fn slots(self) -> &'static [Slot] {
        match self {
            Kind::Icon => ICON_SLOTS,
            Kind::Logo => LOGO_SLOTS,
            Kind::Hero => HERO_SLOTS,
        }
    }
}

/// A box art is drawn in, in the UI's 1080p units (`u` in app.slint), and how it fills it.
struct Slot {
    width: f32,
    height: f32,
    /// `image-fit: cover` (fills and crops) rather than `contain`.
    cover: bool,
}

/// Tiles: the selected icon in the row (`big`) and a recent card (`card-w`, 16:9).
const ICON_SLOTS: &[Slot] = &[
    Slot {
        width: 164.0,
        height: 164.0,
        cover: true,
    },
    Slot {
        width: 300.0,
        height: 168.75,
        cover: true,
    },
];
/// The logo area left of the actions.
const LOGO_SLOTS: &[Slot] = &[Slot {
    width: 760.0,
    height: 260.0,
    cover: false,
}];

/// The whole window, filled and cropped.
const HERO_SLOTS: &[Slot] = &[Slot {
    width: 1920.0,
    height: 1080.0,
    cover: true,
}];

/// Decoded pixels that can cross threads (a Slint Image can't), so backgrounds decode off the
/// UI thread and only become an Image once back on it.
pub enum Pixels {
    Rgb(SharedPixelBuffer<Rgb8Pixel>),
    Rgba(SharedPixelBuffer<Rgba8Pixel>),
}

impl Pixels {
    pub fn into_image(self) -> Image {
        match self {
            Pixels::Rgb(buffer) => Image::from_rgb8(buffer),
            Pixels::Rgba(buffer) => Image::from_rgba8(buffer),
        }
    }
}

/// Screen pixels per UI unit: 1.0 at 1080p, 2.0 at 4K. Measured on the bounding box of all
/// monitors, which is never smaller than any one of them, so a TV next to the PC screen counts.
/// Infinite (never shrink) when Windows would report scaled-down sizes instead of pixels.
pub fn screen_scale() -> f32 {
    let (width, height, aware) = unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
            IsProcessDPIAware() != 0,
        )
    };
    if !aware || width <= 0 || height <= 0 {
        return f32::INFINITY;
    }
    (width as f32 / 1920.0).min(height as f32 / 1080.0)
}

/// The size to keep an image at, or None when it is already no bigger than it is drawn.
fn target_size(width: u32, height: u32, slots: &[Slot], scale: f32) -> Option<(u32, u32)> {
    let (w, h) = (width as f32, height as f32);
    let fit = slots
        .iter()
        .map(|s| {
            let (x, y) = (s.width / w, s.height / h);
            if s.cover { x.max(y) } else { x.min(y) }
        })
        .fold(0.0, f32::max)
        * scale;
    if fit >= 1.0 {
        return None;
    }
    Some(((w * fit).ceil() as u32, (h * fit).ceil() as u32))
}

/// None when the file can't be decoded here (e.g. WebP); the caller falls back to Slint.
pub fn load(path: &Path, kind: Kind, scale: f32) -> Option<Image> {
    decode(path, kind, scale).map(Pixels::into_image)
}

/// Safe to call from any thread.
pub fn decode(path: &Path, kind: Kind, scale: f32) -> Option<Pixels> {
    let decoded = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let has_alpha = decoded.color().has_alpha();
    let decoded = match target_size(decoded.width(), decoded.height(), kind.slots(), scale) {
        Some((w, h)) if has_alpha => shrink_with_alpha(&decoded, w, h),
        Some((w, h)) => decoded.resize_exact(w, h, FilterType::Lanczos3),
        None => decoded,
    };
    Some(if has_alpha {
        let rgba = decoded.into_rgba8();
        Pixels::Rgba(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
        ))
    } else {
        let rgb = decoded.into_rgb8();
        Pixels::Rgb(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
        ))
    })
}

/// Resizing straight RGBA lets the hidden colour of transparent pixels bleed into the edges
/// (dark fringes around logos); weighting colour by alpha first keeps edges clean.
fn shrink_with_alpha(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let mut pixels = image.to_rgba32f();
    for p in pixels.pixels_mut() {
        let a = p.0[3];
        for c in &mut p.0[..3] {
            *c *= a;
        }
    }
    let mut small: Rgba32FImage = imageops::resize(&pixels, width, height, FilterType::Lanczos3);
    for p in small.pixels_mut() {
        // Lanczos overshoots slightly; keep values in range before undoing the weighting.
        let a = p.0[3].clamp(0.0, 1.0);
        p.0[3] = a;
        for c in &mut p.0[..3] {
            *c = if a > 0.0 {
                (*c / a).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }
    DynamicImage::ImageRgba32F(small)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_shrink_to_the_biggest_tile_they_fill() {
        // 600x900 fills a 300x169 recent card at half size; the 164 px tile needs less.
        assert_eq!(target_size(600, 900, ICON_SLOTS, 1.0), Some((300, 450)));
        // On a 4K screen the same card is drawn at 600 px wide: keep the original.
        assert_eq!(target_size(600, 900, ICON_SLOTS, 2.0), None);
    }

    #[test]
    fn backgrounds_shrink_to_fill_the_screen() {
        // Steam's 3840x1240 background fills a 1080p screen at 3345x1080 (cropped at the sides).
        let (w, h) = target_size(3840, 1240, HERO_SLOTS, 1.0).unwrap();
        assert!(h >= 1080 && (1920..3840).contains(&w), "{w}x{h}");
        assert_eq!(
            target_size(3840, 1240, HERO_SLOTS, 2.0),
            None,
            "4K keeps it all"
        );
        assert_eq!(
            target_size(1920, 620, HERO_SLOTS, 1.0),
            None,
            "never enlarged"
        );
    }

    #[test]
    fn art_is_never_enlarged_or_made_smaller_than_drawn() {
        assert_eq!(target_size(100, 100, ICON_SLOTS, 1.0), None);
        assert_eq!(target_size(600, 900, ICON_SLOTS, f32::INFINITY), None);
        let (w, h) = target_size(640, 360, LOGO_SLOTS, 1.0).unwrap();
        assert!(w >= 462 && h >= 260, "logo fits 760x260 at {w}x{h}");
        assert!(w < 640);
    }

    #[test]
    fn transparent_edges_keep_their_colour() {
        // Opaque red next to fully transparent black: the blended edge must stay red, not darken.
        let mut source = image::RgbaImage::new(8, 1);
        for x in 0..4 {
            source.put_pixel(x, 0, image::Rgba([255, 0, 0, 255]));
        }
        let small = shrink_with_alpha(&DynamicImage::ImageRgba8(source), 4, 1).into_rgba8();
        for p in small.pixels().filter(|p| p.0[3] > 0) {
            assert!(p.0[0] >= 250 && p.0[1] == 0 && p.0[2] == 0, "{p:?}");
        }
    }
}
