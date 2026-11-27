# rustos-fb

Pixel drawing over a GOP framebuffer, for the RustOS kernel console.

The bootloader hands the kernel a framebuffer (on QEMU: 1280x800,
Bgr, 4 bytes/pixel, already mapped into the kernel's address space).
This crate owns the raw pixel buffer and knows how to draw text into
it: one 8x16 cell per character, from the embedded 8x8 bitmap font
in `src/font.rs` (each glyph row drawn twice, so the cell is 8 wide
and 16 tall — 160x50 cells at 1280x800).

No dependencies, `no_std`, host-testable: every operation runs
against a caller-supplied buffer, so the unit tests render into a
`Vec<u32>` and assert pixels. The only `unsafe` is the raw pointer
the `Framebuffer` holds — the same shape as the kernel's VGA writer
over `0xb8000`.

## What it does

| Item | Notes |
|---|---|
| `Framebuffer` | raw `*mut u32` pixel buffer + layout; volatile pixel I/O |
| `FrameBufferInfo` / `PixelFormat` | layout (width/height/stride/bytes-per-pixel) and format |
| `draw_glyph` | one 8x16 cell: set bits `fg`, clear bits `bg`; non-printables draw a solid block |
| `scroll_up` | memmove scroll by whole text rows, bottom filled with `bg` |
| `fill` / `clear_cell` / `set_pixel` / `pixel` | whole-buffer fill, one cell, one pixel (bounds-checked) |
| `checksum` / `non_background_pixels` | FNV-1a over the pixels + lit-pixel count — the live proof for a `-display none` boot (the kernel logs both to serial) |

## Formats

Only 32-bit **Rgb** and **Bgr** are supported (what GOP hands out).
`U8` and unknown formats are rejected at construction
(`from_bytes` / `new` return `None`), so the kernel degrades to
serial+VGA instead of writing garbage. A buffer that is too short,
misaligned, or narrower than one text cell is rejected the same way.

## Font

The font is hand-drawn in `src/font.rs`: 95 glyphs (printable ASCII
`0x20`..=`0x7E`), 8 rows each, binary literals, MSB on the left.
There is no font file and no font crate — crates.io is not reachable
from the build VM, so no font crate could be fetched. Capitals and
digits fill the full cell; lowercase sits on rows 2–6 with
descenders (`g`, `j`, `p`, `q`, `y`) dropping to row 7.

## Who uses it

`rustos-tty` draws through this crate; the kernel's `fbcon` module
builds a `Framebuffer` from `BootInfo::framebuffer` and hands it to
the `Terminal`. Nothing else touches pixels directly.

## Tests

`cargo test` — 13 tests: geometry, format rejection, Bgr/Rgb byte
order, pixel round-trip, glyph rendering (including the vertical
doubling), non-printable blocks, checksum determinism, scrolling.
