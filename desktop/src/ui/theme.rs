//! One place for the colours and the couple of numbers that are repeated.
//!
//! Dark, because this is a thing to glance at from a metre away in a dim room
//! while something else has your attention. Levels are the only saturated
//! colour on the screen, so a red meter is the thing your eye lands on.

use gpui::{rgb, Rgba};

pub const BG: u32 = 0x14161a;
pub const PANEL: u32 = 0x1c1f26;
pub const PANEL_ALT: u32 = 0x232732;
pub const BORDER: u32 = 0x2e3440;
/// Used where a line has to separate two areas rather than outline one:
/// the titlebar from the panels below it.
pub const BORDER_STRONG: u32 = 0x3d4658;
pub const TEXT: u32 = 0xe6e9ef;
pub const MUTED: u32 = 0x8b93a7;
pub const ACCENT: u32 = 0x4c9aff;
pub const LIVE: u32 = 0x35c46b;
pub const WARN: u32 = 0xffb020;
pub const DANGER: u32 = 0xff5c5c;
pub const TRACK: u32 = 0x11131a;

/// Green up to a comfortable level, amber where a limiter starts to work, red
/// where it is working hard. The thresholds are the ones the meter ballistics
/// in [`lanmic::meter`] were written for: 1.0 is full scale, and the engine
/// reports up to 2.0.
pub fn level_color(level: f32) -> Rgba {
    if level >= 0.95 {
        rgb(DANGER)
    } else if level >= 0.70 {
        rgb(WARN)
    } else {
        rgb(LIVE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_only_goes_red_where_the_limiter_is_working() {
        assert_eq!(level_color(0.0), rgb(LIVE));
        assert_eq!(level_color(0.69), rgb(LIVE));
        assert_eq!(level_color(0.70), rgb(WARN));
        assert_eq!(level_color(0.94), rgb(WARN));
        assert_eq!(level_color(1.0), rgb(DANGER));
        assert_eq!(level_color(2.0), rgb(DANGER));
    }
}
