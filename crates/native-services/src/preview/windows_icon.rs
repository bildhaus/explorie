//! In-process Windows shell icons. The icon comes from the system image list
//! (jumbo or extra-large) found through `SHGetFileInfoW` and `SHGetImageList`,
//! falling back to the classic large icon, and is read back with
//! `GetIconInfo` and `GetDIBits` so its alpha channel survives. This replaces
//! a PowerShell process per file.
//!
//! The pixel conversion is plain Rust and is unit tested on every platform;
//! only the Win32 calls are Windows-specific.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Straight-alpha RGBA pixels, row-major with the top row first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IconPixels {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Vec<u8>,
}

impl IconPixels {
    /// Convert a top-down 32-bit BGRA bitmap. Icons whose color bitmap has no
    /// alpha at all (legacy 24-bit artwork) take their transparency from the
    /// AND mask, where white marks a transparent pixel.
    pub(super) fn from_bgra(
        width: u32,
        height: u32,
        bgra: &[u8],
        mask: Option<&[u8]>,
    ) -> Option<Self> {
        let length = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if length == 0 || bgra.len() != length || mask.is_some_and(|mask| mask.len() != length) {
            return None;
        }
        let pixels = bgra.as_chunks::<4>().0;
        let has_alpha = pixels.iter().any(|[_, _, _, alpha]| *alpha != 0);
        let mut rgba = Vec::with_capacity(length);
        for (index, [blue, green, red, alpha]) in pixels.iter().enumerate() {
            let alpha = if has_alpha {
                *alpha
            } else if mask.is_some_and(|mask| mask[index * 4] != 0) {
                0
            } else {
                255
            };
            rgba.extend_from_slice(&[*red, *green, *blue, alpha]);
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Right and bottom edges (exclusive) of the pixels that are not fully
    /// transparent, or `None` for a blank icon.
    pub(super) fn visible_extent(&self) -> Option<(u32, u32)> {
        let width = self.width.max(1) as usize;
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, pixel)| pixel[3] != 0)
            .fold(None, |extent, (index, _)| {
                let (x, y) = ((index % width) as u32 + 1, (index / width) as u32 + 1);
                Some(extent.map_or((x, y), |(right, bottom): (u32, u32)| {
                    (right.max(x), bottom.max(y))
                }))
            })
    }

    /// Scale down (never up) to fit in a `limit` square. Filtering happens in
    /// premultiplied alpha so the color of transparent pixels cannot bleed
    /// into the icon's edges.
    pub(super) fn fit_within(self, limit: u32) -> Option<Self> {
        let limit = limit.max(1);
        if self.width <= limit && self.height <= limit {
            return Some(self);
        }
        let scale = f64::from(limit) / f64::from(self.width.max(self.height));
        let width = ((f64::from(self.width) * scale).round() as u32).clamp(1, limit);
        let height = ((f64::from(self.height) * scale).round() as u32).clamp(1, limit);
        let mut premultiplied = self.rgba;
        for pixel in premultiplied.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        let source = image::RgbaImage::from_raw(self.width, self.height, premultiplied)?;
        let mut rgba = image::imageops::resize(
            &source,
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
        .into_raw();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 + alpha / 2)
                    .checked_div(alpha)
                    .map_or(0, |value| value.min(255) as u8);
            }
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Encode as PNG at `output`, replacing any existing file atomically.
    pub(super) fn save_png(&self, output: &Path) -> std::io::Result<()> {
        static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
        let temporary = output.with_extension(format!(
            "png.{}-{}.tmp",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let result = image::save_buffer_with_format(
            &temporary,
            &self.rgba,
            self.width,
            self.height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(std::io::Error::other)
        .and_then(|()| std::fs::rename(&temporary, output));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

/// The shell icon for `path`, or `None` when the shell has none or does not
/// answer within `timeout` (for example an unreachable network path). A call
/// that times out finishes on its own thread and its result is dropped.
#[cfg(windows)]
pub(super) fn shell_icon_with_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> Option<IconPixels> {
    let path = path.to_path_buf();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("explorie-shell-icon".into())
        .spawn(move || {
            let _ = sender.send(shell_icon(&path));
        })
        .ok()?;
    receiver.recv_timeout(timeout).ok().flatten()
}

#[cfg(windows)]
use windows_sys::Win32::{
    Graphics::Gdi::HBITMAP,
    UI::WindowsAndMessaging::{HICON, ICONINFO},
};

/// Largest icon edge accepted from the shell; real icons are at most 256.
#[cfg(windows)]
const MAX_SHELL_ICON_EDGE: u32 = 1024;

/// Edge of the extra-large system image list, and the largest artwork the
/// jumbo list draws in its top-left corner for types without a 256-pixel icon.
#[cfg(windows)]
const EXTRA_LARGE_ICON_EDGE: u32 = 48;

/// `IID_IImageList` ({46EB5926-582E-4017-9FDF-E8998DAA0950}).
#[cfg(windows)]
const IID_IIMAGELIST: windows_sys::core::GUID = windows_sys::core::GUID {
    data1: 0x46eb_5926,
    data2: 0x582e,
    data3: 0x4017,
    data4: [0x9f, 0xdf, 0xe8, 0x99, 0x8d, 0xaa, 0x09, 0x50],
};

#[cfg(windows)]
fn shell_icon(path: &Path) -> Option<IconPixels> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHGetFileInfoW,
        SHIL_EXTRALARGE, SHIL_JUMBO,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let _apartment = ComApartment::enter();

    // SAFETY: `wide` is NUL-terminated and `info` is a zeroed SHFILEINFOW of
    // the size passed. SHGFI_SYSICONINDEX returns no handle to release.
    let mut info = unsafe { std::mem::zeroed::<SHFILEINFOW>() };
    let found = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if found != 0 {
        // SAFETY: `iIcon` is an index into the system image lists.
        if let Some(jumbo) = unsafe { image_list_icon(SHIL_JUMBO, info.iIcon) } {
            // Types without 256-pixel artwork are drawn small in the corner of
            // the jumbo canvas; the extra-large list shows those better.
            if jumbo.visible_extent().is_some_and(|(right, bottom)| {
                right > EXTRA_LARGE_ICON_EDGE || bottom > EXTRA_LARGE_ICON_EDGE
            }) {
                return Some(jumbo);
            }
        }
        // SAFETY: As above.
        if let Some(extra_large) = unsafe { image_list_icon(SHIL_EXTRALARGE, info.iIcon) } {
            return Some(extra_large);
        }
    }

    // SAFETY: As above; SHGFI_ICON hands over an icon released below.
    let mut info = unsafe { std::mem::zeroed::<SHFILEINFOW>() };
    let found = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if found == 0 || info.hIcon.is_null() {
        return None;
    }
    // SAFETY: `hIcon` is a valid icon owned by this function.
    let pixels = unsafe { icon_pixels(info.hIcon) };
    unsafe { DestroyIcon(info.hIcon) };
    pixels
}

/// The icon at `index` of one of the system image lists. The `IImageList`
/// pointer `SHGetImageList` returns is documented to double as an
/// `HIMAGELIST`, so no COM method beyond `Release` is needed.
///
/// # Safety
///
/// COM must be initialized on the calling thread.
#[cfg(windows)]
unsafe fn image_list_icon(list: u32, index: i32) -> Option<IconPixels> {
    use windows_sys::Win32::UI::Controls::{HIMAGELIST, ILD_TRANSPARENT, ImageList_GetIcon};
    use windows_sys::Win32::UI::Shell::SHGetImageList;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

    let mut image_list = std::ptr::null_mut::<core::ffi::c_void>();
    // SAFETY: The out-pointer is valid; a success result carries a reference
    // released below.
    let result = unsafe { SHGetImageList(list as i32, &IID_IIMAGELIST, &mut image_list) };
    if result < 0 || image_list.is_null() {
        return None;
    }
    // SAFETY: The image list stays alive until released after this call.
    let icon = unsafe { ImageList_GetIcon(image_list as HIMAGELIST, index, ILD_TRANSPARENT) };
    // SAFETY: `image_list` is a live COM object owning one reference.
    unsafe { release_com_object(image_list) };
    if icon.is_null() {
        return None;
    }
    // SAFETY: `icon` is a valid icon owned by this function.
    let pixels = unsafe { icon_pixels(icon) };
    unsafe { DestroyIcon(icon) };
    pixels
}

/// Call `IUnknown::Release`, the third entry of every COM vtable.
///
/// # Safety
///
/// `object` must point to a live COM object whose reference the caller owns.
#[cfg(windows)]
unsafe fn release_com_object(object: *mut core::ffi::c_void) {
    type Release = unsafe extern "system" fn(*mut core::ffi::c_void) -> u32;
    // SAFETY: A COM object starts with a pointer to its vtable, whose first
    // three entries are QueryInterface, AddRef and Release.
    unsafe {
        let vtable = *object.cast::<*const Release>();
        (*vtable.add(2))(object);
    }
}

/// Read an icon's pixels, releasing the bitmaps `GetIconInfo` creates.
///
/// # Safety
///
/// `icon` must be a valid icon handle.
#[cfg(windows)]
unsafe fn icon_pixels(icon: HICON) -> Option<IconPixels> {
    use windows_sys::Win32::Graphics::Gdi::DeleteObject;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo;

    // SAFETY: `info` is a valid out-pointer; on success the caller owns both
    // bitmaps, deleted below.
    let mut info = unsafe { std::mem::zeroed::<ICONINFO>() };
    if unsafe { GetIconInfo(icon, &mut info) } == 0 {
        return None;
    }
    // SAFETY: The bitmaps come from GetIconInfo and are not selected into any
    // device context.
    let pixels = unsafe { color_icon_pixels(&info) };
    for bitmap in [info.hbmColor, info.hbmMask] {
        if !bitmap.is_null() {
            // SAFETY: Each bitmap is owned here and deleted exactly once.
            unsafe { DeleteObject(bitmap) };
        }
    }
    pixels
}

/// # Safety
///
/// The bitmaps in `info` must be valid and not selected into a device context.
#[cfg(windows)]
unsafe fn color_icon_pixels(info: &ICONINFO) -> Option<IconPixels> {
    use windows_sys::Win32::Graphics::Gdi::{BITMAP, GetObjectW};

    // Monochrome icons (no color bitmap) do not occur in the system image
    // lists; leave them to the drawn fallback.
    if info.hbmColor.is_null() {
        return None;
    }
    // SAFETY: `bitmap` is a valid out-buffer of the size passed.
    let mut bitmap = unsafe { std::mem::zeroed::<BITMAP>() };
    let copied = unsafe {
        GetObjectW(
            info.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            (&raw mut bitmap).cast(),
        )
    };
    if copied == 0 {
        return None;
    }
    let width = u32::try_from(bitmap.bmWidth).ok()?;
    let height = u32::try_from(bitmap.bmHeight).ok()?;
    if width == 0 || height == 0 || width > MAX_SHELL_ICON_EDGE || height > MAX_SHELL_ICON_EDGE {
        return None;
    }
    // SAFETY: Both bitmaps are valid, unselected GDI bitmaps.
    let color = unsafe { dib_pixels(info.hbmColor, width, height) }?;
    let mask = unsafe { dib_pixels(info.hbmMask, width, height) };
    IconPixels::from_bgra(width, height, &color, mask.as_deref())
}

/// Copy a bitmap as top-down 32-bit BGRA. A monochrome mask converts to black
/// (opaque) and white (transparent) pixels.
///
/// # Safety
///
/// `bitmap` must be null or a valid bitmap not selected into a device context.
#[cfg(windows)]
unsafe fn dib_pixels(bitmap: HBITMAP, width: u32, height: u32) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetDIBits, ReleaseDC,
    };

    if bitmap.is_null() {
        return None;
    }
    let length = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    // SAFETY: A zeroed BITMAPINFO is valid; the fields GetDIBits reads are set
    // below.
    let mut header = unsafe { std::mem::zeroed::<BITMAPINFO>() };
    header.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: i32::try_from(width).ok()?,
        // A negative height requests top-down rows.
        biHeight: -i32::try_from(height).ok()?,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let mut pixels = vec![0_u8; length];
    // SAFETY: The screen DC is released below; `pixels` holds exactly
    // `height` rows of 32-bit pixels as described by `header`.
    let copied = unsafe {
        let dc = GetDC(std::ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        let copied = GetDIBits(
            dc,
            bitmap,
            0,
            height,
            pixels.as_mut_ptr().cast(),
            &mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(std::ptr::null_mut(), dc);
        copied
    };
    (u32::try_from(copied).ok() == Some(height)).then_some(pixels)
}

/// Keeps COM initialized (single-threaded apartment, as the shell expects) on
/// the current thread for as long as it lives.
#[cfg(windows)]
struct ComApartment {
    initialized: bool,
}

#[cfg(windows)]
impl ComApartment {
    fn enter() -> Self {
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // SAFETY: Balanced by CoUninitialize in Drop when it succeeds (S_OK or
        // S_FALSE); a thread already in another apartment keeps its own.
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        Self {
            initialized: result >= 0,
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: Balances the successful CoInitializeEx in `enter`.
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_with_alpha_keeps_its_alpha_and_swaps_channels() {
        let bgra = [10, 20, 30, 0, 40, 50, 60, 128];
        let white_mask = [255; 8];
        let pixels = IconPixels::from_bgra(2, 1, &bgra, Some(&white_mask)).unwrap();
        assert_eq!(pixels.rgba, vec![30, 20, 10, 0, 60, 50, 40, 128]);
    }

    #[test]
    fn legacy_icons_take_transparency_from_the_and_mask() {
        let bgra = [10, 20, 30, 0, 40, 50, 60, 0];
        let mask = [0, 0, 0, 0, 255, 255, 255, 0];
        let pixels = IconPixels::from_bgra(2, 1, &bgra, Some(&mask)).unwrap();
        assert_eq!(pixels.rgba, vec![30, 20, 10, 255, 60, 50, 40, 0]);
        let unmasked = IconPixels::from_bgra(2, 1, &bgra, None).unwrap();
        assert_eq!(unmasked.rgba[3], 255);
        assert_eq!(unmasked.rgba[7], 255);
    }

    #[test]
    fn mismatched_buffers_are_rejected() {
        assert!(IconPixels::from_bgra(2, 2, &[0; 12], None).is_none());
        assert!(IconPixels::from_bgra(1, 1, &[0; 4], Some(&[0; 8])).is_none());
        assert!(IconPixels::from_bgra(0, 1, &[], None).is_none());
    }

    fn square(edge: u32, visible: u32) -> IconPixels {
        let mut rgba = vec![0; (edge * edge * 4) as usize];
        for y in 0..visible {
            for x in 0..visible {
                let index = ((y * edge + x) * 4) as usize;
                rgba[index..index + 4].copy_from_slice(&[200, 100, 50, 255]);
            }
        }
        IconPixels {
            width: edge,
            height: edge,
            rgba,
        }
    }

    #[test]
    fn visible_extent_detects_small_artwork_in_a_jumbo_canvas() {
        assert_eq!(square(256, 48).visible_extent(), Some((48, 48)));
        assert_eq!(square(256, 256).visible_extent(), Some((256, 256)));
        assert_eq!(square(16, 0).visible_extent(), None);
    }

    #[test]
    fn downscaling_filters_in_premultiplied_alpha() {
        // A red half next to transparent black: straight-alpha filtering would
        // darken the boundary, premultiplied filtering keeps it red.
        let mut rgba = Vec::new();
        for _ in 0..4 {
            rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        let pixels = IconPixels {
            width: 4,
            height: 4,
            rgba,
        }
        .fit_within(2)
        .unwrap();
        assert_eq!((pixels.width, pixels.height), (2, 2));
        for pixel in pixels.rgba.as_chunks::<4>().0 {
            if pixel[3] > 0 {
                assert_eq!(&pixel[..3], &[255, 0, 0], "edge colour bled: {pixel:?}");
            }
        }

        let small = square(48, 48);
        assert_eq!(small.clone().fit_within(128), Some(small));
    }

    #[test]
    fn icons_save_as_png() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("icon.png");
        square(256, 256)
            .fit_within(128)
            .unwrap()
            .save_png(&output)
            .unwrap();
        assert_eq!(image::image_dimensions(&output).unwrap(), (128, 128));
        let decoded = image::open(&output).unwrap().into_rgba8();
        assert_eq!(decoded.get_pixel(64, 64).0, [200, 100, 50, 255]);
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            1,
            "no temporary file should remain"
        );
    }
}
