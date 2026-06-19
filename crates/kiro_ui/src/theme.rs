//! Centralized colors for KiroUI (Catppuccin Mocha-ish palette).

use gpui::{rgb, Rgba};

pub const BASE: u32 = 0x1e1e2e;
pub const MANTLE: u32 = 0x181825;
pub const CRUST: u32 = 0x11111b;
pub const SURFACE0: u32 = 0x313244;
pub const SURFACE1: u32 = 0x45475a;
pub const SURFACE2: u32 = 0x585b70;
pub const TEXT: u32 = 0xcdd6f4;
pub const SUBTEXT: u32 = 0xa6adc8;
pub const OVERLAY: u32 = 0x6c7086;
pub const BLUE: u32 = 0x89b4fa;
pub const GREEN: u32 = 0xa6e3a1;
pub const YELLOW: u32 = 0xf9e2af;
pub const RED: u32 = 0xf38ba8;
pub const MAUVE: u32 = 0xcba6f7;
pub const TEAL: u32 = 0x94e2d5;

pub fn c(hex: u32) -> Rgba {
    rgb(hex)
}
