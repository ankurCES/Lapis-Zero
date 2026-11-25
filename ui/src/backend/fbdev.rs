//! fbdev backend for the CardputerZero LCD (ST7789v3 over SPI).
//!
//! The BSP drives the panel with a DRM driver (`panel-mipi-dbi-m5stack`); what we
//! open is its fbdev *emulation*, 16 bpp RGB565, which udev links as `/dev/fb_lcd`
//! (usually also `/dev/fb0`). Three consequences, all learned on the hardware:
//!
//! * With `fbcon=map:off` nothing ever enables the display pipeline, so we
//!   unblank and re-activate the mode ourselves on open.
//! * While another process holds the DRM device with its own framebuffer (the
//!   BSP's `fb_load` splash) fbdev writes are invisible and those two ioctls
//!   silently do nothing, so the image masks `fb_load.service`.
//! * `/dev/fb0` is not always the panel. Until the DRM modules load (about 12 s
//!   into boot) it is the firmware's 16 bpp simple-framebuffer, which the kernel
//!   then removes; a process that latched onto it draws into nothing for good.
//!   [`check_lcd_mode`] therefore accepts only the panel's own geometry.
//!
//! The RGB888 -> RGB565 conversion and the stride-aware blit are portable and
//! unit-tested on any host; every Linux syscall sits behind
//! `#[cfg(target_os = "linux")]`.

#[cfg(target_os = "linux")]
use crate::gfx::canvas::Canvas;
#[cfg(target_os = "linux")]
use crate::gfx::canvas::{HEIGHT, WIDTH};

// ioctl request numbers for the Linux fbdev ABI (`include/uapi/linux/fb.h`).
#[cfg(target_os = "linux")]
const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
#[cfg(target_os = "linux")]
const FBIOPUT_VSCREENINFO: libc::c_ulong = 0x4601;
#[cfg(target_os = "linux")]
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
#[cfg(target_os = "linux")]
const FBIOBLANK: libc::c_ulong = 0x4611;
/// `FB_ACTIVATE_NOW | FB_ACTIVATE_FORCE`: re-run `set_par` even if nothing changed.
#[cfg(target_os = "linux")]
const FB_ACTIVATE_NOW_FORCE: u32 = 128;

/// Candidate nodes for the LCD, most specific first.
pub const LCD_PATHS: [&str; 3] = ["/dev/fb_lcd", "/dev/fb0", "/dev/fb1"];

/// Packs one 8-bit RGB triple into RGB565 (`rrrrr gggggg bbbbb`).
///
/// Matches the ST7789v3 framebuffer pixel format the CardputerZero BSP exposes.
pub fn rgb888_to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3)
}

/// Converts an RGB888 byte slice (`len == dst.len() * 3`) into packed RGB565.
///
/// Allocation-free: the caller owns `dst`. Returns silently if the input length
/// is not exactly three bytes per output pixel.
pub fn convert_rgb888_to_rgb565(src: &[u8], dst: &mut [u16]) {
    let n = dst.len().min(src.len() / 3);
    for (i, px) in dst.iter_mut().take(n).enumerate() {
        *px = rgb888_to_rgb565(src[i * 3], src[i * 3 + 1], src[i * 3 + 2]);
    }
}

/// Blits `src` (row-major RGB565 pixels, `width` per row) into `dst` (framebuffer
/// memory, `stride_pixels` per row) little-endian. Rows wider than the visible
/// width are skipped so the write stays stride-safe.
pub fn blit_rgb565_rows(
    src: &[u16],
    dst: &mut [u8],
    width: usize,
    height: usize,
    stride_pixels: usize,
) {
    let stride_bytes = stride_pixels * 2;
    for y in 0..height {
        let src_row = &src[y * width..(y + 1) * width];
        let dst_start = y * stride_bytes;
        let Some(dst_row) = dst.get_mut(dst_start..dst_start + width * 2) else {
            return;
        };
        for (i, px) in src_row.iter().enumerate() {
            let b = px.to_le_bytes();
            dst_row[i * 2] = b[0];
            dst_row[i * 2 + 1] = b[1];
        }
    }
}

/// Accepts only the CardputerZero panel: 16 bpp at exactly 320x170.
///
/// The firmware simple-framebuffer and an HDMI console are also 16 bpp, so the
/// pixel depth alone does not identify the LCD (see the module docs).
pub fn check_lcd_mode(xres: u32, yres: u32, bits_per_pixel: u32) -> Result<(), String> {
    use crate::gfx::canvas::{HEIGHT, WIDTH};
    if bits_per_pixel != 16 {
        return Err(format!(
            "{xres}x{yres} at {bits_per_pixel} bpp is not 16 bpp RGB565"
        ));
    }
    if xres != WIDTH as u32 || yres != HEIGHT as u32 {
        return Err(format!(
            "{xres}x{yres} is not the {WIDTH}x{HEIGHT} LCD (firmware or HDMI framebuffer?)"
        ));
    }
    Ok(())
}

/// The driver name in `fb_fix_screeninfo.id` (NUL-padded, at most 16 bytes).
pub fn fix_id_str(id: &[u8]) -> String {
    let end = id.iter().position(|&b| b == 0).unwrap_or(id.len());
    String::from_utf8_lossy(&id[..end]).into_owned()
}

/// The `struct fb_bitfield` the kernel reports for each colour channel.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// The `struct fb_var_screeninfo` returned by `FBIOGET_VSCREENINFO`. Field order
/// is the kernel ABI; `#[repr(C)]` reproduces its padding exactly on aarch64.
#[repr(C)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

/// The `struct fb_fix_screeninfo` returned by `FBIOGET_FSCREENINFO`. We only read
/// `line_length` and `smem_len`, but the layout must match so offsets line up.
#[repr(C)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: usize,
    smem_len: u32,
    #[allow(dead_code)]
    fb_type: u32,
    #[allow(dead_code)]
    type_aux: u32,
    #[allow(dead_code)]
    visual: u32,
    #[allow(dead_code)]
    xpanstep: u16,
    #[allow(dead_code)]
    ypanstep: u16,
    #[allow(dead_code)]
    ywrapstep: u16,
    line_length: u32,
    #[allow(dead_code)]
    mmio_start: usize,
    #[allow(dead_code)]
    mmio_len: u32,
    #[allow(dead_code)]
    accel: u32,
    #[allow(dead_code)]
    capabilities: u16,
    #[allow(dead_code)]
    reserved: [u16; 2],
}

/// An mmap-backed `/dev/fb0` display backend.
#[cfg(target_os = "linux")]
pub struct FbBackend {
    _file: std::fs::File,
    map: std::ptr::NonNull<u8>,
    map_len: usize,
    stride_pixels: usize,
    visible_h: usize,
    rgb565: Vec<u16>,
    id: String,
}

#[cfg(target_os = "linux")]
impl FbBackend {
    /// Opens the first usable node in [`LCD_PATHS`]; the error names every miss.
    pub fn open_lcd() -> std::io::Result<(Self, &'static str)> {
        let mut misses = String::new();
        for path in LCD_PATHS {
            match Self::open(path) {
                Ok(fb) => return Ok((fb, path)),
                Err(e) => misses.push_str(&format!("{path}: {e}; ")),
            }
        }
        Err(std::io::Error::other(misses))
    }

    /// Opens `path` (usually `/dev/fb_lcd`), queries the fixed + variable screen
    /// info and memory-maps the framebuffer. Fails with a descriptive error if
    /// the panel is not 16 bpp RGB565.
    pub fn open(path: &str) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let fd = file.as_raw_fd();

        // Variable screen info: resolution and pixel format.
        let mut var: FbVarScreeninfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut var) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Nothing below may change the device until it is known to be the LCD.
        check_lcd_mode(var.xres, var.yres, var.bits_per_pixel)
            .map_err(|why| std::io::Error::other(format!("{path}: {why}")))?;

        // Fixed screen info: physical memory size and line stride.
        let mut fix: FbFixScreeninfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut fix) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let line_length = fix.line_length as usize;
        if line_length < WIDTH as usize * 2 {
            return Err(std::io::Error::other(format!(
                "{path}: line_length {line_length} too small for 320x170 RGB565"
            )));
        }
        // The node is the LCD. Take the display: unblank, then force the fbdev
        // mode back onto the CRTC. This is what initialises the panel when no DRM
        // master ever ran (proven on the device, 2026-11-24, in this order: the
        // two ioctls first, then the mmap). Both calls report success even when
        // a DRM master makes them no-ops.
        unsafe {
            libc::ioctl(fd, FBIOBLANK, 0 as libc::c_ulong);
            var.activate = FB_ACTIVATE_NOW_FORCE;
            libc::ioctl(fd, FBIOPUT_VSCREENINFO, &var);
        }

        let stride_pixels = line_length / 2;
        let vheight = var.yres_virtual.max(var.yres) as usize;
        let map_len = (line_length * vheight.max(HEIGHT as usize)).max(fix.smem_len as usize);

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }

        let visible_h = (var.yres as usize).min(HEIGHT as usize);
        Ok(Self {
            _file: file,
            map: std::ptr::NonNull::new(ptr as *mut u8).unwrap(),
            map_len,
            stride_pixels,
            visible_h,
            rgb565: vec![0u16; (WIDTH * HEIGHT) as usize],
            id: fix_id_str(&fix.id),
        })
    }

    /// The kernel driver behind this framebuffer, e.g. `panel-mipi-dbid`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the visible height of the panel (`min(yres, 170)`).
    pub fn visible_height(&self) -> usize {
        self.visible_h
    }

    /// Returns the framebuffer stride in pixels (`line_length / 2`).
    pub fn stride_pixels(&self) -> usize {
        self.stride_pixels
    }
}

#[cfg(target_os = "linux")]
impl Drop for FbBackend {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map.as_ptr() as *mut libc::c_void, self.map_len) };
    }
}

#[cfg(target_os = "linux")]
impl super::DisplayBackend for FbBackend {
    fn push_frame(&mut self, canvas: &Canvas) {
        convert_rgb888_to_rgb565(canvas.pixels(), &mut self.rgb565);
        let fb = unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr(), self.map_len) };
        blit_rgb565_rows(
            &self.rgb565,
            fb,
            WIDTH as usize,
            HEIGHT as usize,
            self.stride_pixels,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ioctl structs must match `include/uapi/linux/fb.h` byte for byte on a
    /// 64-bit kernel, or FBIOGET_* would write past them / be read at wrong offsets.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn fbdev_abi_struct_layouts() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<FbBitfield>(), 12);
        assert_eq!(size_of::<FbVarScreeninfo>(), 160);
        assert_eq!(offset_of!(FbVarScreeninfo, bits_per_pixel), 24);
        assert_eq!(offset_of!(FbVarScreeninfo, activate), 84);
        assert_eq!(size_of::<FbFixScreeninfo>(), 80);
        assert_eq!(offset_of!(FbFixScreeninfo, smem_len), 24);
        assert_eq!(offset_of!(FbFixScreeninfo, line_length), 48);
    }

    #[test]
    fn only_the_panel_geometry_is_accepted() {
        assert_eq!(check_lcd_mode(320, 170, 16), Ok(()));
        // The firmware simple-framebuffer that /dev/fb0 is early in boot (16 bpp,
        // which is why depth alone let lapis-ui latch onto it on the hardware).
        for (w, h) in [(656, 416), (640, 480), (720, 480), (1920, 1080), (170, 320)] {
            let err = check_lcd_mode(w, h, 16).unwrap_err();
            assert!(err.contains("is not the 320x170 LCD"), "{err}");
        }
        assert!(check_lcd_mode(320, 170, 32).unwrap_err().contains("32 bpp"));
    }

    #[test]
    fn fix_id_stops_at_the_first_nul() {
        assert_eq!(fix_id_str(b"panel-mipi-dbid\0"), "panel-mipi-dbid");
        assert_eq!(fix_id_str(b"simple\0\0\0\0\0\0\0\0\0\0"), "simple");
        assert_eq!(fix_id_str(b"0123456789abcdef"), "0123456789abcdef");
    }

    #[test]
    fn rgb565_packs_5_6_5() {
        assert_eq!(rgb888_to_rgb565(0xFF, 0xFF, 0xFF), 0xFFFF);
        assert_eq!(rgb888_to_rgb565(0x00, 0x00, 0x00), 0x0000);
        // Pure red -> top 5 bits set, no green/blue.
        assert_eq!(rgb888_to_rgb565(0xFF, 0x00, 0x00), 0xF800);
        // Pure green -> middle 6 bits set.
        assert_eq!(rgb888_to_rgb565(0x00, 0xFF, 0x00), 0x07E0);
        // Pure blue -> bottom 5 bits set.
        assert_eq!(rgb888_to_rgb565(0x00, 0x00, 0xFF), 0x001F);
    }

    #[test]
    fn convert_rgb888_shrinks_to_rgb565() {
        let src = [0xFFu8, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF];
        let mut dst = [0u16; 3];
        convert_rgb888_to_rgb565(&src, &mut dst);
        assert_eq!(dst, [0xF800, 0x07E0, 0x001F]);
    }

    #[test]
    fn convert_ignores_short_input() {
        // Only 3 bytes (one pixel) supplied for a 2-pixel buffer: the second
        // pixel must stay untouched.
        let src = [0xFFu8, 0xFF, 0xFF];
        let mut dst = [0x1234u16, 0x5678];
        convert_rgb888_to_rgb565(&src, &mut dst);
        assert_eq!(dst[0], 0xFFFF);
        assert_eq!(dst[1], 0x5678);
    }

    #[test]
    fn blit_respects_tight_stride() {
        let src = [0xF800u16, 0x07E0];
        let mut dst = [0u8; 4];
        blit_rgb565_rows(&src, &mut dst, 2, 1, 2);
        assert_eq!(&dst, &[0x00, 0xF8, 0xE0, 0x07]);
    }

    #[test]
    fn blit_skips_stride_padding() {
        // 2 visible pixels per row, 4-pixel stride: the second half of each row
        // must remain untouched (test initialises it to 0xAA first).
        let src = [0xFFFFu16, 0x0000, 0xFFFF, 0x0000];
        let mut dst = vec![0xAAu8; 2 * 8];
        blit_rgb565_rows(&src, &mut dst, 2, 2, 4);
        // Row 0 visible: FF FF 00 00, padding: AA AA AA AA AA AA AA AA
        assert_eq!(&dst[0..4], &[0xFF, 0xFF, 0x00, 0x00]);
        assert_eq!(&dst[4..8], &[0xAA; 4]);
        // Row 1 starts at byte 8.
        assert_eq!(&dst[8..12], &[0xFF, 0xFF, 0x00, 0x00]);
        assert_eq!(&dst[12..16], &[0xAA; 4]);
    }

    #[test]
    fn blit_bails_on_short_buffer() {
        let src = [0xFFFFu16; 4];
        let mut dst = [0u8; 2]; // too small for one row
        blit_rgb565_rows(&src, &mut dst, 4, 1, 4);
        // No panic; buffer left untouched.
        assert_eq!(dst, [0, 0]);
    }
}
