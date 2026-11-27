//! `rustos-fb`: pixel drawing over a GOP framebuffer.
//!
//! The bootloader hands the kernel a framebuffer (on QEMU: 1280x800,
//! Bgr, 4 bytes/pixel at `0x80000000`, already mapped into the
//! kernel's address space by the bootloader). This crate owns the raw
//! pixel buffer and knows how to draw text into it: one 8x16 cell per
//! character, from the embedded 8x8 bitmap font in [`font`] (each
//! glyph row drawn twice, so the cell is 8 wide and 16 tall).
//!
//! The crate is `no_std` and has no dependencies. It is host-testable:
//! every operation runs against a caller-supplied buffer, so the unit
//! tests render into a `Vec<u32>` on the stack and assert pixels.
//! The only `unsafe` is the raw pointer the [`Framebuffer`] holds —
//! the same shape as the kernel's VGA writer over `0xb8000`.
//!
//! Supported formats are 32-bit Rgb and Bgr only (what GOP hands
//! out). `U8` and `Unknown` formats are rejected at construction, so
//! the kernel degrades to serial+VGA instead of writing garbage.

#![cfg_attr(not(test), no_std)]

pub mod font;

use font::{glyph_for, GLYPH_HEIGHT, GLYPH_WIDTH};

/// Width of one text cell, in pixels.
pub const CELL_WIDTH: usize = GLYPH_WIDTH;
/// Height of one text cell, in pixels: each 8-row glyph is drawn
/// doubled vertically.
pub const CELL_HEIGHT: usize = GLYPH_HEIGHT * 2;

/// A colour as 8-bit red/green/blue components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Build a colour from components.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Black.
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    /// White.
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);
}

/// Pixel format of a framebuffer: the byte order of one pixel.
/// (Same variants as `bootloader_api::info::PixelFormat`, kept
/// separate so this crate does not depend on the bootloader crate —
/// the kernel converts at the boundary in `fbcon`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// One byte red, then green, then blue (fourth byte unused).
    Rgb,
    /// One byte blue, then green, then red (fourth byte unused).
    Bgr,
    /// Any format this crate cannot draw into (grayscale, unknown
    /// bit positions, …): construction rejects it.
    Unsupported,
}

/// Layout and pixel format of a framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct FrameBufferInfo {
    /// Total size of the buffer, in bytes.
    pub byte_len: usize,
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Colour format of each pixel.
    pub pixel_format: PixelFormat,
    /// Bytes per pixel (must be 4 for this crate).
    pub bytes_per_pixel: usize,
    /// Pixels between the start of one line and the next (may exceed
    /// `width` when the firmware pads lines).
    pub stride: usize,
}

/// A framebuffer: a raw pixel buffer plus its layout.
///
/// Pixels are written with volatile stores (this is memory-mapped
/// I/O — the compiler must not elide or reorder them) and read back
/// with volatile loads. Scrolling uses `core::ptr::copy` (memmove)
/// for the bulk move, which cannot be elided because the destination
/// is behind a raw pointer.
pub struct Framebuffer {
    pixels: *mut u32,
    pixel_count: usize,
    info: FrameBufferInfo,
}

impl Framebuffer {
    /// Build a framebuffer over `buffer` (the bootloader's
    /// `buffer_mut()` slice, or a test's `Vec<u32>` as bytes).
    ///
    /// Returns `None` (rejects the framebuffer) when the format is
    /// not 32-bit Rgb/Bgr, the dimensions are zero, `stride` is
    /// narrower than `width`, the buffer is too short, or its start
    /// is not 4-byte aligned. A rejected framebuffer is a normal
    /// outcome, not a panic: the kernel runs serial+VGA-only.
    pub fn from_bytes(buffer: &mut [u8], info: FrameBufferInfo) -> Option<Self> {
        if buffer.as_ptr() as usize % 4 != 0 {
            return None;
        }
        if buffer.len() < info.byte_len {
            return None;
        }
        // SAFETY: alignment was just checked, and the length check
        // above guarantees at least `byte_len` bytes.
        unsafe { Self::new(buffer.as_mut_ptr() as *mut u32, buffer.len() / 4, info) }
    }

    /// Build a framebuffer over a raw `u32` pixel pointer.
    ///
    /// # Safety
    ///
    /// `pixels` must point at `pixel_count` valid, writable `u32`
    /// pixels for the lifetime of this `Framebuffer`, unaliased by
    /// any other live reference.
    pub unsafe fn new(
        pixels: *mut u32,
        pixel_count: usize,
        info: FrameBufferInfo,
    ) -> Option<Self> {
        if info.bytes_per_pixel != 4 {
            return None;
        }
        if !matches!(info.pixel_format, PixelFormat::Rgb | PixelFormat::Bgr) {
            return None;
        }
        if info.width == 0 || info.height == 0 || info.stride < info.width {
            return None;
        }
        // At least one text cell must fit, or the terminal layer
        // would index an empty shadow buffer.
        if info.width < CELL_WIDTH || info.height < CELL_HEIGHT {
            return None;
        }
        let needed = info.stride.saturating_mul(info.height - 1).saturating_add(info.width);
        if pixel_count < needed {
            return None;
        }
        Some(Self {
            pixels,
            pixel_count,
            info,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.info.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.info.height
    }

    /// Pixels per line (may exceed `width`).
    pub fn stride(&self) -> usize {
        self.info.stride
    }

    /// Pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.info.pixel_format
    }

    /// Text columns that fit: `width / 8`.
    pub fn cols(&self) -> usize {
        self.info.width / CELL_WIDTH
    }

    /// Text rows that fit: `height / 16`.
    pub fn rows(&self) -> usize {
        self.info.height / CELL_HEIGHT
    }

    /// Pack a colour into one pixel word for this framebuffer's
    /// format. The fourth byte is always zero.
    pub fn pack(&self, color: Rgb) -> u32 {
        match self.info.pixel_format {
            PixelFormat::Rgb => {
                (color.r as u32) | ((color.g as u32) << 8) | ((color.b as u32) << 16)
            }
            _ => (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16),
        }
    }

    /// Unpack one pixel word back into a colour (the reverse of
    /// [`pack`](Self::pack); the fourth byte is ignored).
    pub fn unpack(&self, pixel: u32) -> Rgb {
        match self.info.pixel_format {
            PixelFormat::Rgb => Rgb::new(
                pixel as u8,
                (pixel >> 8) as u8,
                (pixel >> 16) as u8,
            ),
            _ => Rgb::new(
                (pixel >> 16) as u8,
                (pixel >> 8) as u8,
                pixel as u8,
            ),
        }
    }

    /// Set one pixel, bounds-checked (out-of-range writes are
    /// dropped, never a fault).
    pub fn set_pixel(&mut self, x: usize, y: usize, color: Rgb) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let idx = y * self.info.stride + x;
        unsafe {
            core::ptr::write_volatile(self.pixels.add(idx), self.pack(color));
        }
    }

    /// Read one pixel back (`None` out of range). Used by tests and
    /// diagnostics; the kernel's live path never reads pixels except
    /// for [`checksum`](Self::checksum).
    pub fn pixel(&self, x: usize, y: usize) -> Option<Rgb> {
        if x >= self.info.width || y >= self.info.height {
            return None;
        }
        let idx = y * self.info.stride + x;
        let word = unsafe { core::ptr::read_volatile(self.pixels.add(idx)) };
        Some(self.unpack(word))
    }

    /// Fill the whole buffer (including stride padding) with one
    /// colour.
    pub fn fill(&mut self, color: Rgb) {
        let word = self.pack(color);
        for i in 0..self.pixel_count {
            unsafe {
                core::ptr::write_volatile(self.pixels.add(i), word);
            }
        }
    }

    /// Fill one pixel row with one colour.
    fn fill_row(&mut self, y: usize, color: Rgb) {
        if y >= self.info.height {
            return;
        }
        let word = self.pack(color);
        let start = y * self.info.stride;
        for i in 0..self.info.stride {
            unsafe {
                core::ptr::write_volatile(self.pixels.add(start + i), word);
            }
        }
    }

    /// Draw one glyph into cell (`col`, `row`): an 8x16 cell, each
    /// of the font's 8 rows drawn twice. Set bits get `fg`, clear
    /// bits get `bg`. A cell outside the grid is a no-op.
    /// Non-printable bytes draw [`BLOCK`] (solid block).
    pub fn draw_glyph(&mut self, col: usize, row: usize, ch: u8, fg: Rgb, bg: Rgb) {
        if col >= self.cols() || row >= self.rows() {
            return;
        }
        let glyph = glyph_for(ch);
        let x0 = col * CELL_WIDTH;
        let y0 = row * CELL_HEIGHT;
        for (gy, bits) in glyph.iter().enumerate() {
            for gx in 0..GLYPH_WIDTH {
                let on = bits & (0x80 >> gx) != 0;
                let color = if on { fg } else { bg };
                self.set_pixel(x0 + gx, y0 + gy * 2, color);
                self.set_pixel(x0 + gx, y0 + gy * 2 + 1, color);
            }
        }
    }

    /// Blank one cell with `bg`.
    pub fn clear_cell(&mut self, col: usize, row: usize, bg: Rgb) {
        if col >= self.cols() || row >= self.rows() {
            return;
        }
        let x0 = col * CELL_WIDTH;
        let y0 = row * CELL_HEIGHT;
        for y in y0..y0 + CELL_HEIGHT {
            for x in x0..x0 + CELL_WIDTH {
                self.set_pixel(x, y, bg);
            }
        }
    }

    /// Scroll the pixel buffer up by `lines` text rows (16 pixels
    /// each), filling the vacated rows at the bottom with `bg`.
    /// `lines == 0` is a no-op; scrolling by the whole screen (or
    /// more) just clears it.
    pub fn scroll_up(&mut self, lines: usize, bg: Rgb) {
        if lines == 0 {
            return;
        }
        let px = lines * CELL_HEIGHT;
        if px >= self.info.height {
            self.fill(bg);
            return;
        }
        let rows_to_move = self.info.height - px;
        let count = rows_to_move * self.info.stride;
        unsafe {
            let src = self.pixels.add(px * self.info.stride);
            // Overlapping copy (destination below source): memmove
            // semantics, correct for the overlap.
            core::ptr::copy(src, self.pixels, count);
        }
        for y in rows_to_move..self.info.height {
            self.fill_row(y, bg);
        }
    }

    /// FNV-1a (64-bit) checksum over every pixel word. The live
    /// proof for a `-display none` boot: the kernel logs this to
    /// serial after the boot lines (and again ~5 s in), so a
    /// changing, non-zero checksum proves the framebuffer is really
    /// being rendered into, not just allocated.
    pub fn checksum(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for i in 0..self.pixel_count {
            let word = unsafe { core::ptr::read_volatile(self.pixels.add(i)) } as u64;
            hash ^= word;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Count of pixels that differ from `bg` — i.e. how much text
    /// (or cursor) is actually drawn. Zero on a freshly cleared
    /// screen; the serial log pairs it with [`checksum`](Self::checksum).
    pub fn non_background_pixels(&self, bg: Rgb) -> u64 {
        let word = self.pack(bg);
        let mut n: u64 = 0;
        for i in 0..self.pixel_count {
            if unsafe { core::ptr::read_volatile(self.pixels.add(i)) } != word {
                n += 1;
            }
        }
        n
    }
}

// `Framebuffer` holds a raw pointer: it is safe to move between
// threads in the kernel's sense (the kernel is single-threaded
// between IRQs; the TTY lock serialises access). Mark it Send so the
// kernel can hold it in a static; it is never shared unsynchronised.
unsafe impl Send for Framebuffer {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::BLOCK;

    /// Standard QEMU/GOP geometry: 1280x800, Bgr, 4 bytes/pixel.
    fn qemu_info() -> FrameBufferInfo {
        FrameBufferInfo {
            byte_len: 1280 * 800 * 4,
            width: 1280,
            height: 800,
            pixel_format: PixelFormat::Bgr,
            bytes_per_pixel: 4,
            stride: 1280,
        }
    }

    /// A small framebuffer (80x48 px = 10 cols x 3 rows) over a
    /// `Vec<u32>`, for tests that need to see whole rows.
    fn small_fb() -> (Vec<u32>, Framebuffer) {
        let info = FrameBufferInfo {
            byte_len: 80 * 48 * 4,
            width: 80,
            height: 48,
            pixel_format: PixelFormat::Bgr,
            bytes_per_pixel: 4,
            stride: 80,
        };
        let mut buf = vec![0u32; 80 * 48];
        let fb = unsafe { Framebuffer::new(buf.as_mut_ptr(), buf.len(), info) }.unwrap();
        (buf, fb)
    }

    #[test]
    fn qemu_geometry_is_160_by_50_cells() {
        let mut buf = vec![0u32; 1280 * 800];
        let fb = unsafe { Framebuffer::new(buf.as_mut_ptr(), buf.len(), qemu_info()) }.unwrap();
        assert_eq!(fb.cols(), 160);
        assert_eq!(fb.rows(), 50);
        assert_eq!(fb.width(), 1280);
        assert_eq!(fb.height(), 800);
    }

    #[test]
    fn from_bytes_rejects_bad_formats_and_short_buffers() {
        let mut buf = vec![0u8; 1280 * 800 * 4];
        // 3 bytes/pixel: rejected.
        let mut bad = qemu_info();
        bad.bytes_per_pixel = 3;
        assert!(Framebuffer::from_bytes(&mut buf, bad).is_none());
        // Grayscale: rejected.
        let mut bad = qemu_info();
        bad.pixel_format = PixelFormat::Unsupported;
        assert!(Framebuffer::from_bytes(&mut buf, bad).is_none());
        // Zero width: rejected.
        let mut bad = qemu_info();
        bad.width = 0;
        assert!(Framebuffer::from_bytes(&mut buf, bad).is_none());
        // Stride narrower than width: rejected.
        let mut bad = qemu_info();
        bad.stride = 100;
        assert!(Framebuffer::from_bytes(&mut buf, bad).is_none());
        // Short buffer: rejected.
        let mut short = vec![0u8; 100];
        assert!(Framebuffer::from_bytes(&mut short, qemu_info()).is_none());
        // The good one constructs.
        assert!(Framebuffer::from_bytes(&mut buf, qemu_info()).is_some());
    }

    #[test]
    fn pack_matches_bgr_and_rgb_byte_order() {
        // One text cell (8x16 px) — the smallest framebuffer the
        // constructor accepts.
        let info = FrameBufferInfo {
            byte_len: 8 * 16 * 4,
            width: 8,
            height: 16,
            pixel_format: PixelFormat::Bgr,
            bytes_per_pixel: 4,
            stride: 8,
        };
        let mut buf = vec![0u32; 8 * 16];
        let bgr = unsafe { Framebuffer::new(buf.as_mut_ptr(), buf.len(), info) }.unwrap();
        // Bgr: low byte blue, then green, then red.
        assert_eq!(bgr.pack(Rgb::new(0x11, 0x22, 0x33)), 0x0011_2233);
        let mut rgb_info = info;
        rgb_info.pixel_format = PixelFormat::Rgb;
        let rgb = unsafe { Framebuffer::new(buf.as_mut_ptr(), buf.len(), rgb_info) }.unwrap();
        assert_eq!(rgb.pack(Rgb::new(0x11, 0x22, 0x33)), 0x0033_2211);
        // Round trip through unpack.
        assert_eq!(bgr.unpack(bgr.pack(Rgb::new(9, 8, 7))), Rgb::new(9, 8, 7));
        assert_eq!(rgb.unpack(rgb.pack(Rgb::new(9, 8, 7))), Rgb::new(9, 8, 7));
    }

    #[test]
    fn set_and_read_pixel_round_trips_and_bounds_check() {
        let (_buf, mut fb) = small_fb();
        fb.set_pixel(3, 2, Rgb::new(1, 2, 3));
        assert_eq!(fb.pixel(3, 2), Some(Rgb::new(1, 2, 3)));
        assert_eq!(fb.pixel(0, 0), Some(Rgb::BLACK));
        // Out of range: dropped, never a fault, reads back None.
        fb.set_pixel(80, 0, Rgb::WHITE);
        fb.set_pixel(0, 48, Rgb::WHITE);
        assert_eq!(fb.pixel(80, 0), None);
        assert_eq!(fb.pixel(0, 48), None);
    }

    #[test]
    fn draw_glyph_renders_a_pixels_and_doubles_rows() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        fb.draw_glyph(0, 0, b'A', Rgb::WHITE, Rgb::BLACK);
        // 'A' row 0 is 0b00111100: pixels x=2..=5 set, x=0,1,6,7 clear.
        // Each glyph row is drawn twice (y and y+1).
        assert_eq!(fb.pixel(2, 0), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(5, 0), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(2, 1), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(0, 0), Some(Rgb::BLACK));
        assert_eq!(fb.pixel(7, 0), Some(Rgb::BLACK));
        // Row 1 of 'A' is 0b01000010: x=1 and x=6 set, at y=2 and y=3.
        assert_eq!(fb.pixel(1, 2), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(6, 3), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(3, 2), Some(Rgb::BLACK));
        // The cell is 16 tall: the last glyph row (row 7, blank for
        // 'A') lands at y=14,15 — still background.
        assert_eq!(fb.pixel(2, 14), Some(Rgb::BLACK));
        assert_eq!(fb.pixel(2, 15), Some(Rgb::BLACK));
    }

    #[test]
    fn draw_glyph_outside_the_grid_is_a_noop() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        fb.draw_glyph(10, 0, b'A', Rgb::WHITE, Rgb::BLACK);
        fb.draw_glyph(0, 3, b'A', Rgb::WHITE, Rgb::BLACK);
        assert_eq!(fb.non_background_pixels(Rgb::BLACK), 0);
    }

    #[test]
    fn draw_glyph_non_printable_is_a_solid_block() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        fb.draw_glyph(0, 0, 0x01, Rgb::WHITE, Rgb::BLACK);
        // BLOCK is all rows set: every pixel of the cell is lit.
        assert_eq!(fb.pixel(0, 0), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(7, 15), Some(Rgb::WHITE));
        assert_eq!(fb.pixel(3, 7), Some(Rgb::WHITE));
        // The next cell is untouched.
        assert_eq!(fb.pixel(8, 0), Some(Rgb::BLACK));
        // The BLOCK constant itself is what the glyph resolves to.
        assert_eq!(glyph_for(0x01), BLOCK);
    }

    #[test]
    fn fill_and_checksum_are_deterministic_and_content_sensitive() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        let c0 = fb.checksum();
        assert_eq!(fb.checksum(), c0);
        assert_eq!(fb.non_background_pixels(Rgb::BLACK), 0);
        fb.draw_glyph(0, 0, b'X', Rgb::WHITE, Rgb::BLACK);
        let c1 = fb.checksum();
        assert_ne!(c1, c0);
        assert_eq!(fb.checksum(), c1);
        assert!(fb.non_background_pixels(Rgb::BLACK) > 0);
        fb.fill(Rgb::BLACK);
        assert_eq!(fb.checksum(), c0);
        assert_eq!(fb.non_background_pixels(Rgb::BLACK), 0);
    }

    #[test]
    fn scroll_up_moves_content_and_clears_the_bottom() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        fb.draw_glyph(0, 1, b'A', Rgb::WHITE, Rgb::BLACK);
        fb.draw_glyph(0, 2, b'B', Rgb::WHITE, Rgb::BLACK);
        fb.scroll_up(1, Rgb::BLACK);
        // 'A' moved from row 1 to row 0: its top-left set pixel
        // (x=2, y=0 of the cell) is now at pixel y=0.
        assert_eq!(fb.pixel(2, 0), Some(Rgb::WHITE));
        // 'B' moved from row 2 to row 1.
        assert_eq!(fb.pixel(2, 16), Some(Rgb::WHITE));
        // Bottom row (row 2) is cleared.
        assert_eq!(fb.pixel(2, 32), Some(Rgb::BLACK));
        // Scrolling by the whole screen clears everything.
        fb.scroll_up(3, Rgb::BLACK);
        assert_eq!(fb.non_background_pixels(Rgb::BLACK), 0);
        // Zero lines is a no-op.
        fb.draw_glyph(0, 0, b'A', Rgb::WHITE, Rgb::BLACK);
        let c = fb.checksum();
        fb.scroll_up(0, Rgb::BLACK);
        assert_eq!(fb.checksum(), c);
    }

    #[test]
    fn clear_cell_blanks_one_cell_only() {
        let (_buf, mut fb) = small_fb();
        fb.fill(Rgb::BLACK);
        fb.draw_glyph(0, 0, b'A', Rgb::WHITE, Rgb::BLACK);
        fb.draw_glyph(1, 0, b'B', Rgb::WHITE, Rgb::BLACK);
        fb.clear_cell(0, 0, Rgb::BLACK);
        assert_eq!(fb.pixel(2, 0), Some(Rgb::BLACK));
        assert_eq!(fb.pixel(8 + 2, 0), Some(Rgb::WHITE));
    }
}
