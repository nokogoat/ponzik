//! Pad controllers as clip launchers: which pad of which device is which
//! launcher button, and the bytes that light its LED in the clip's color.
//!
//! Everything here is a pure function of the device and the button: the
//! interface detects a device by its MIDI port name ([`detect`]), maps its
//! notes and controllers to [`GridButton`]s ([`button_of`]) and lights the
//! pads with [`led_message`]. Protocols, from the makers' public references:
//!
//! - Novation Launchpad X, Mini MK3 and Pro MK3, in programmer mode (entered
//!   with a SysEx, [`init_message`]): pad `10 × row + column` (row 1 at the
//!   bottom), the right column's scene buttons as controllers 89 to 19, the
//!   top row as controllers 91 to 98. Colors as RGB SysEx (`03` specs), the
//!   queued blink as a flashing palette color (`01` specs).
//! - Novation Launchpad Mini (MK1, MK2) and the original Launchpad: X-Y
//!   layout, note `16 × row + column` (row 0 at the top), scene buttons at
//!   column 8; colors are red and green levels in the velocity.
//! - Akai APC Mini (MK1): notes 0 to 63 from the bottom left, track buttons
//!   64 to 71, scene buttons 82 to 89; LEDs green, red or yellow, steady or
//!   blinking, by velocity.
//! - Akai APC Mini MK2: same layout, track buttons 100 to 107, scene buttons
//!   112 to 119; the velocity is a palette color, the channel its brightness
//!   or blink.
//! - Akai APC40 and APC40 MK2, in Ableton mode ([`init_message`]): 8 by 5
//!   clip buttons (notes 53 to 57 by channel on the MK1, notes 0 to 39 from
//!   the bottom left on the MK2), clip stop on note 52 by channel, scene
//!   launch on notes 82 to 86, stop all on note 81.
//! - Ableton Push 1 and 2 in user mode, and any other pad grid: the generic
//!   8 by 8 note grid, notes 36 to 99 from the bottom left, colors as palette
//!   velocities (Push), none otherwise.
//!
//! None of this has been checked on the hardware yet: the tables follow the
//! documents. A device that does not answer as documented still launches
//! through MIDI learn.

use crate::midi_map::CtlSource;

/// A known pad controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GridDevice {
    LaunchpadX,
    LaunchpadMiniMk3,
    LaunchpadProMk3,
    /// Launchpad Mini MK1/MK2 and the original Launchpad (X-Y layout).
    LaunchpadXy,
    ApcMini,
    ApcMiniMk2,
    Apc40,
    Apc40Mk2,
    /// Ableton Push 1 or 2 in user mode.
    Push,
    /// Any 8 by 8 grid sending notes 36 to 99.
    Generic,
}

impl GridDevice {
    pub const ALL: [GridDevice; 10] = [
        Self::LaunchpadX,
        Self::LaunchpadMiniMk3,
        Self::LaunchpadProMk3,
        Self::LaunchpadXy,
        Self::ApcMini,
        Self::ApcMiniMk2,
        Self::Apc40,
        Self::Apc40Mk2,
        Self::Push,
        Self::Generic,
    ];

    /// Name shown in the interface (a product name, not translated).
    pub fn name(self) -> &'static str {
        match self {
            Self::LaunchpadX => "Launchpad X",
            Self::LaunchpadMiniMk3 => "Launchpad Mini MK3",
            Self::LaunchpadProMk3 => "Launchpad Pro MK3",
            Self::LaunchpadXy => "Launchpad (MK1/MK2)",
            Self::ApcMini => "APC Mini",
            Self::ApcMiniMk2 => "APC Mini MK2",
            Self::Apc40 => "APC40",
            Self::Apc40Mk2 => "APC40 MK2",
            Self::Push => "Push",
            Self::Generic => "8x8",
        }
    }

    /// Clip buttons: columns and rows.
    pub fn size(self) -> (usize, usize) {
        match self {
            Self::Apc40 | Self::Apc40Mk2 => (8, 5),
            _ => (8, 8),
        }
    }

    /// Novation device byte of the programmer mode SysEx.
    fn novation_id(self) -> Option<u8> {
        match self {
            Self::LaunchpadX => Some(0x0C),
            Self::LaunchpadMiniMk3 => Some(0x0D),
            Self::LaunchpadProMk3 => Some(0x0E),
            _ => None,
        }
    }
}

/// Recognizes a pad controller by the name of its MIDI port. Launchpads
/// with a "DAW" port and a "MIDI" port are recognized on the MIDI one.
pub fn detect(port: &str) -> Option<GridDevice> {
    let n = port.to_ascii_lowercase();
    if n.contains("daw") && n.contains("launchpad") {
        return None;
    }
    let has = |s: &str| n.contains(s);
    Some(if has("launchpad x") || has("lpx") {
        GridDevice::LaunchpadX
    } else if has("lpminimk3") || has("launchpad mini mk3") {
        GridDevice::LaunchpadMiniMk3
    } else if has("lppromk3") || has("launchpad pro mk3") {
        GridDevice::LaunchpadProMk3
    } else if has("launchpad") && !has("pro") {
        GridDevice::LaunchpadXy
    } else if has("apc mini mk2") || has("apc mini mkii") {
        GridDevice::ApcMiniMk2
    } else if has("apc mini") || has("apc_mini") {
        GridDevice::ApcMini
    } else if has("apc40 mkii") || has("apc40 mk2") {
        GridDevice::Apc40Mk2
    } else if has("apc40") {
        GridDevice::Apc40
    } else if has("ableton push") || has("push 2") || has("push2") {
        GridDevice::Push
    } else {
        return None;
    })
}

/// A launcher button of a pad controller. Rows count from the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GridButton {
    Clip { col: usize, row: usize },
    Scene(usize),
    Stop(usize),
    StopAll,
}

/// The button a message comes from, if it is one.
pub fn button_of(dev: GridDevice, src: CtlSource) -> Option<GridButton> {
    let (cols, rows) = dev.size();
    let clip = |col: usize, row: usize| (col < cols && row < rows).then_some(GridButton::Clip { col, row });
    match (dev, src) {
        (GridDevice::LaunchpadX | GridDevice::LaunchpadMiniMk3 | GridDevice::LaunchpadProMk3, CtlSource::Note { key, .. }) => {
            let (r, c) = (key / 10, key % 10);
            if (1..=8).contains(&r) && (1..=8).contains(&c) { clip(c as usize - 1, 8 - r as usize) } else { None }
        }
        (GridDevice::LaunchpadX | GridDevice::LaunchpadMiniMk3 | GridDevice::LaunchpadProMk3, CtlSource::Cc { cc, .. }) => {
            match cc {
                // Right column: scenes, top to bottom.
                19..=89 if cc % 10 == 9 => Some(GridButton::Scene((8 - cc / 10) as usize)),
                // Top row: the last button stops everything, the others stop their column.
                98 => Some(GridButton::StopAll),
                91..=97 => Some(GridButton::Stop((cc - 91) as usize)),
                _ => None,
            }
        }
        (GridDevice::LaunchpadXy, CtlSource::Note { key, .. }) => {
            let (r, c) = ((key / 16) as usize, (key % 16) as usize);
            match c {
                0..=7 => clip(c, r),
                8 if r < 8 => Some(GridButton::Scene(r)),
                _ => None,
            }
        }
        (GridDevice::LaunchpadXy, CtlSource::Cc { cc, .. }) => match cc {
            111 => Some(GridButton::StopAll),
            104..=110 => Some(GridButton::Stop((cc - 104) as usize)),
            _ => None,
        },
        (GridDevice::ApcMini | GridDevice::ApcMiniMk2, CtlSource::Note { key, .. }) => {
            let (tracks, scenes) = if dev == GridDevice::ApcMini { (64, 82) } else { (100, 112) };
            match key {
                0..=63 => clip((key % 8) as usize, 7 - (key / 8) as usize),
                k if (tracks..tracks + 8).contains(&k) => Some(GridButton::Stop((k - tracks) as usize)),
                // The last scene button stops everything ("Stop all clips").
                k if k == scenes + 7 => Some(GridButton::StopAll),
                k if (scenes..scenes + 7).contains(&k) => Some(GridButton::Scene((k - scenes) as usize)),
                _ => None,
            }
        }
        (GridDevice::Apc40 | GridDevice::Apc40Mk2, CtlSource::Note { channel, key }) => match key {
            53..=57 if dev == GridDevice::Apc40 => clip(channel as usize, (key - 53) as usize),
            0..=39 if dev == GridDevice::Apc40Mk2 => clip((key % 8) as usize, 4 - (key / 8) as usize),
            52 => Some(GridButton::Stop(channel as usize)).filter(|_| channel < 8),
            81 => Some(GridButton::StopAll),
            82..=86 => Some(GridButton::Scene((key - 82) as usize)),
            _ => None,
        },
        (GridDevice::Push | GridDevice::Generic, CtlSource::Note { key, .. }) => {
            let k = key.checked_sub(36)?;
            if k < 64 { clip((k % 8) as usize, 7 - (k / 8) as usize) } else { None }
        }
        _ => None,
    }
}

/// The message a button sends (its note or controller), for its LED.
pub fn source_of(dev: GridDevice, b: GridButton) -> Option<CtlSource> {
    let note = |channel: u8, key: usize| Some(CtlSource::Note { channel, key: key as u8 });
    let cc = |cc: usize| Some(CtlSource::Cc { channel: 0, cc: cc as u8 });
    let (cols, rows) = dev.size();
    if let GridButton::Clip { col, row } = b
        && (col >= cols || row >= rows)
    {
        return None;
    }
    match (dev, b) {
        (GridDevice::LaunchpadX | GridDevice::LaunchpadMiniMk3 | GridDevice::LaunchpadProMk3, b) => match b {
            GridButton::Clip { col, row } => note(0, 10 * (8 - row) + col + 1),
            GridButton::Scene(r) if r < 8 => cc(10 * (8 - r) + 9),
            GridButton::Stop(c) if c < 7 => cc(91 + c),
            GridButton::StopAll => cc(98),
            _ => None,
        },
        (GridDevice::LaunchpadXy, b) => match b {
            GridButton::Clip { col, row } => note(0, 16 * row + col),
            GridButton::Scene(r) if r < 8 => note(0, 16 * r + 8),
            GridButton::Stop(c) if c < 7 => cc(104 + c),
            GridButton::StopAll => cc(111),
            _ => None,
        },
        (GridDevice::ApcMini | GridDevice::ApcMiniMk2, b) => {
            let (tracks, scenes) = if dev == GridDevice::ApcMini { (64, 82) } else { (100, 112) };
            match b {
                GridButton::Clip { col, row } => note(0, (7 - row) * 8 + col),
                GridButton::Stop(c) if c < 8 => note(0, tracks + c),
                GridButton::Scene(r) if r < 7 => note(0, scenes + r),
                GridButton::StopAll => note(0, scenes + 7),
                _ => None,
            }
        }
        (GridDevice::Apc40, b) => match b {
            GridButton::Clip { col, row } => note(col as u8, 53 + row),
            GridButton::Stop(c) if c < 8 => note(c as u8, 52),
            GridButton::Scene(r) if r < 5 => note(0, 82 + r),
            GridButton::StopAll => note(0, 81),
            _ => None,
        },
        (GridDevice::Apc40Mk2, b) => match b {
            GridButton::Clip { col, row } => note(0, (4 - row) * 8 + col),
            GridButton::Stop(c) if c < 8 => note(c as u8, 52),
            GridButton::Scene(r) if r < 5 => note(0, 82 + r),
            GridButton::StopAll => note(0, 81),
            _ => None,
        },
        (GridDevice::Push | GridDevice::Generic, GridButton::Clip { col, row }) => note(0, 36 + (7 - row) * 8 + col),
        _ => None,
    }
}

/// What a pad shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Led {
    Off,
    /// A clip that does not play: its color, dimmed.
    Idle(u32),
    /// A clip that plays: its color.
    Playing(u32),
    /// A clip waiting to start (or a stop waiting): its color, blinking.
    Queued(u32),
}

/// Hues of the Novation (and Akai MK2, Push) 128-color palette: the bright
/// entry of each group of four, with its color. The entry after it is the
/// same hue at full level, two after it dimmer.
const PALETTE: [(u8, u32); 15] = [
    (5, 0xff0000),
    (9, 0xff5400),
    (13, 0xffff00),
    (17, 0x88ff00),
    (21, 0x00ff00),
    (25, 0x00ff55),
    (29, 0x00ff99),
    (33, 0x00ffff),
    (37, 0x00a9ff),
    (41, 0x0055ff),
    (45, 0x0000ff),
    (49, 0x5400ff),
    (53, 0xff00ff),
    (57, 0xff0054),
    (3, 0xffffff),
];

/// Nearest palette entry to an RGB color (0xRRGGBB); `dim` takes the dimmer one.
pub fn palette_index(rgb: u32, dim: bool) -> u8 {
    let (r, g, b) = ((rgb >> 16 & 0xff) as i32, (rgb >> 8 & 0xff) as i32, (rgb & 0xff) as i32);
    let max = r.max(g).max(b).max(1);
    // Compare hues: both colors scaled to full brightness.
    let (r, g, b) = (r * 255 / max, g * 255 / max, b * 255 / max);
    let dist = |c: u32| {
        let (cr, cg, cb) = ((c >> 16 & 0xff) as i32, (c >> 8 & 0xff) as i32, (c & 0xff) as i32);
        (r - cr).pow(2) + (g - cg).pow(2) + (b - cb).pow(2)
    };
    let (i, _) = PALETTE.iter().min_by_key(|(_, c)| dist(*c)).copied().unwrap_or((21, 0));
    match (i, dim) {
        (3, true) => 1,
        (i, true) => i + 2,
        (i, false) => i,
    }
}

/// Red and green levels (0..3) of a color on a two-color Launchpad or APC.
fn red_green(rgb: u32) -> (u8, u8) {
    let (r, g) = ((rgb >> 16 & 0xff) as u32, (rgb >> 8 & 0xff) as u32);
    let max = r.max(g).max(1);
    ((r * 3 / max) as u8, (g * 3 / max) as u8)
}

/// Bytes to send when the device opens: Launchpad programmer mode, APC40
/// Ableton mode (its LEDs answer the host), nothing for the others.
pub fn init_message(dev: GridDevice) -> Vec<u8> {
    if let Some(id) = dev.novation_id() {
        return vec![0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x0E, 0x01, 0xF7];
    }
    match dev {
        GridDevice::Apc40 => vec![0xF0, 0x47, 0x7F, 0x73, 0x60, 0x00, 0x04, 0x41, 0x08, 0x02, 0x01, 0xF7],
        GridDevice::Apc40Mk2 => vec![0xF0, 0x47, 0x7F, 0x29, 0x60, 0x00, 0x04, 0x41, 0x08, 0x02, 0x01, 0xF7],
        // Reset: all LEDs off.
        GridDevice::LaunchpadXy => vec![0xB0, 0x00, 0x00],
        _ => Vec::new(),
    }
}

/// Bytes to send when Ponzik lets the device go: Launchpads back to live mode.
pub fn exit_message(dev: GridDevice) -> Vec<u8> {
    match dev.novation_id() {
        Some(id) => vec![0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x0E, 0x00, 0xF7],
        None => Vec::new(),
    }
}

/// Bytes that light a button's LED, empty when the device has none to light.
pub fn led_message(dev: GridDevice, b: GridButton, led: Led) -> Vec<u8> {
    let Some(src) = source_of(dev, b) else { return Vec::new() };
    let (status, channel, index) = match src {
        CtlSource::Note { channel, key } => (0x90u8, channel, key),
        CtlSource::Cc { channel, cc } => (0xB0u8, channel, cc),
        CtlSource::PitchBend { .. } => return Vec::new(),
    };
    let short = |ch: u8, v: u8| vec![status | (ch & 0x0F), index, v & 0x7F];
    if let Some(id) = dev.novation_id() {
        let head = [0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x03];
        let rgb = |c: u32, k: u32| -> [u8; 3] {
            let ch = |s: u32| ((c >> s & 0xff) * k / 255 / 2) as u8;
            [ch(16), ch(8), ch(0)]
        };
        let spec: Vec<u8> = match led {
            Led::Off => vec![0x00, index, 0],
            Led::Idle(c) => [vec![0x03, index], rgb(c, 60).to_vec()].concat(),
            Led::Playing(c) => [vec![0x03, index], rgb(c, 255).to_vec()].concat(),
            // Flashing between the color and off.
            Led::Queued(c) => vec![0x01, index, 0, palette_index(c, false)],
        };
        return [head.to_vec(), spec, vec![0xF7]].concat();
    }
    match dev {
        GridDevice::LaunchpadXy => {
            // Velocity: green × 16 + red, + 12 (copy and clear flags); 8 instead flashes.
            let (v, flash) = match led {
                Led::Off => (0, false),
                Led::Idle(c) => {
                    let (r, g) = red_green(c);
                    (16 * g.min(1) + r.min(1), false)
                }
                Led::Playing(c) => {
                    let (r, g) = red_green(c);
                    (16 * g + r, false)
                }
                Led::Queued(c) => {
                    let (r, g) = red_green(c);
                    (16 * g + r, true)
                }
            };
            short(0, v + if flash { 8 } else { 12 })
        }
        GridDevice::ApcMini | GridDevice::Apc40 => {
            // 1 green, 3 red, 5 yellow; +1 blinks.
            let base = |c: u32| match red_green(c) {
                (r, g) if r >= 2 && g >= 2 => 5,
                (r, g) if r > g => 3,
                _ => 1,
            };
            let v = match led {
                Led::Off => 0,
                Led::Idle(c) if matches!(b, GridButton::Clip { .. }) => base(c),
                Led::Idle(_) => 0,
                Led::Playing(c) => base(c),
                Led::Queued(c) => base(c) + 1,
            };
            short(channel, v)
        }
        GridDevice::ApcMiniMk2 | GridDevice::Apc40Mk2 | GridDevice::Push => {
            // Channel: 0 to 6 brightness (MK2), 6 full; 14 blinks at a quarter note.
            let full = if dev == GridDevice::Push { 0 } else { 6 };
            let side = !matches!(b, GridButton::Clip { .. });
            match led {
                Led::Off => short(channel.max(if side { channel } else { 0 }), 0),
                // Single-color side buttons: on or off.
                _ if side => short(channel, matches!(led, Led::Playing(_) | Led::Queued(_)) as u8),
                Led::Idle(c) => short(if dev == GridDevice::Push { 0 } else { 2 }, palette_index(c, dev == GridDevice::Push)),
                Led::Playing(c) => short(full, palette_index(c, false)),
                Led::Queued(c) => short(if dev == GridDevice::Push { 0 } else { 14 }, palette_index(c, false)),
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: u8) -> CtlSource {
        CtlSource::Note { channel: 0, key }
    }

    #[test]
    fn devices_are_recognized_by_their_port() {
        assert_eq!(detect("Launchpad X:Launchpad X LPX MIDI In 24:1"), Some(GridDevice::LaunchpadX));
        assert_eq!(detect("Launchpad X LPX DAW In"), None, "the DAW port is not the grid");
        assert_eq!(detect("Launchpad Mini MK3 LPMiniMK3 MIDI"), Some(GridDevice::LaunchpadMiniMk3));
        assert_eq!(detect("Launchpad Pro MK3 LPProMK3 MIDI"), Some(GridDevice::LaunchpadProMk3));
        assert_eq!(detect("Launchpad Mini"), Some(GridDevice::LaunchpadXy));
        assert_eq!(detect("APC MINI MIDI 1"), Some(GridDevice::ApcMini));
        assert_eq!(detect("APC mini mk2 Control"), Some(GridDevice::ApcMiniMk2));
        assert_eq!(detect("Akai APC40"), Some(GridDevice::Apc40));
        assert_eq!(detect("APC40 mkII"), Some(GridDevice::Apc40Mk2));
        assert_eq!(detect("Ableton Push 2 Live Port"), Some(GridDevice::Push));
        assert_eq!(detect("Arturia KeyStep"), None);
    }

    #[test]
    fn every_clip_button_maps_both_ways() {
        for dev in GridDevice::ALL {
            let (cols, rows) = dev.size();
            let mut seen = std::collections::HashSet::new();
            for col in 0..cols {
                for row in 0..rows {
                    let b = GridButton::Clip { col, row };
                    let src = source_of(dev, b).unwrap_or_else(|| panic!("{dev:?} {b:?}"));
                    assert!(seen.insert(src), "{dev:?}: two pads send {src:?}");
                    assert_eq!(button_of(dev, src), Some(b), "{dev:?} {src:?}");
                }
            }
            for b in [GridButton::Scene(0), GridButton::Stop(0), GridButton::StopAll] {
                if let Some(src) = source_of(dev, b) {
                    assert_eq!(button_of(dev, src), Some(b), "{dev:?} {b:?}");
                    assert!(!seen.contains(&src), "{dev:?}: {b:?} is also a clip pad");
                }
            }
        }
    }

    #[test]
    fn layouts_follow_the_documents() {
        // Launchpad X: 11 bottom left, 88 top right, scene 0 is controller 89.
        assert_eq!(button_of(GridDevice::LaunchpadX, note(11)), Some(GridButton::Clip { col: 0, row: 7 }));
        assert_eq!(button_of(GridDevice::LaunchpadX, note(88)), Some(GridButton::Clip { col: 7, row: 0 }));
        assert_eq!(button_of(GridDevice::LaunchpadX, CtlSource::Cc { channel: 0, cc: 89 }), Some(GridButton::Scene(0)));
        assert_eq!(button_of(GridDevice::LaunchpadX, CtlSource::Cc { channel: 0, cc: 19 }), Some(GridButton::Scene(7)));
        assert_eq!(button_of(GridDevice::LaunchpadX, note(10)), None);
        // X-Y: 0 top left, 119 bottom right, column 8 the scenes.
        assert_eq!(button_of(GridDevice::LaunchpadXy, note(0)), Some(GridButton::Clip { col: 0, row: 0 }));
        assert_eq!(button_of(GridDevice::LaunchpadXy, note(119)), Some(GridButton::Clip { col: 7, row: 7 }));
        assert_eq!(button_of(GridDevice::LaunchpadXy, note(24)), Some(GridButton::Scene(1)));
        // APC Mini: 0 bottom left, 63 top right.
        assert_eq!(button_of(GridDevice::ApcMini, note(0)), Some(GridButton::Clip { col: 0, row: 7 }));
        assert_eq!(button_of(GridDevice::ApcMini, note(63)), Some(GridButton::Clip { col: 7, row: 0 }));
        assert_eq!(button_of(GridDevice::ApcMini, note(64)), Some(GridButton::Stop(0)));
        assert_eq!(button_of(GridDevice::ApcMini, note(89)), Some(GridButton::StopAll));
        assert_eq!(button_of(GridDevice::ApcMiniMk2, note(112)), Some(GridButton::Scene(0)));
        // APC40: the channel is the track.
        assert_eq!(button_of(GridDevice::Apc40, CtlSource::Note { channel: 3, key: 55 }), Some(GridButton::Clip { col: 3, row: 2 }));
        assert_eq!(button_of(GridDevice::Apc40, CtlSource::Note { channel: 5, key: 52 }), Some(GridButton::Stop(5)));
        assert_eq!(button_of(GridDevice::Apc40Mk2, note(32)), Some(GridButton::Clip { col: 0, row: 0 }));
        // Generic grid: 36 bottom left.
        assert_eq!(button_of(GridDevice::Generic, note(36)), Some(GridButton::Clip { col: 0, row: 7 }));
        assert_eq!(button_of(GridDevice::Generic, note(99)), Some(GridButton::Clip { col: 7, row: 0 }));
        assert_eq!(button_of(GridDevice::Generic, note(35)), None);
    }

    #[test]
    fn leds_light_in_the_clip_color() {
        let b = GridButton::Clip { col: 0, row: 7 };
        // Launchpad X: programmer mode, then RGB SysEx at full and dim levels.
        assert_eq!(init_message(GridDevice::LaunchpadX), [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0C, 0x0E, 0x01, 0xF7]);
        assert_eq!(exit_message(GridDevice::LaunchpadMiniMk3)[7], 0x00);
        let on = led_message(GridDevice::LaunchpadX, b, Led::Playing(0xff8000));
        assert_eq!(on, [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0C, 0x03, 0x03, 11, 127, 64, 0, 0xF7]);
        let idle = led_message(GridDevice::LaunchpadX, b, Led::Idle(0xff8000));
        assert!(idle[9] < 40 && idle[9] > 0);
        let blink = led_message(GridDevice::LaunchpadX, b, Led::Queued(0x00ff00));
        assert_eq!(blink, [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0C, 0x03, 0x01, 11, 0, 21, 0xF7]);
        assert_eq!(led_message(GridDevice::LaunchpadX, b, Led::Off)[7..10], [0x00, 11, 0]);
        // APC Mini: green 1, red 3, yellow 5, blinking + 1.
        assert_eq!(led_message(GridDevice::ApcMini, b, Led::Playing(0x3fd45a)), [0x90, 0, 1]);
        assert_eq!(led_message(GridDevice::ApcMini, b, Led::Queued(0xf04040)), [0x90, 0, 4]);
        assert_eq!(led_message(GridDevice::ApcMini, b, Led::Playing(0xf0e040)), [0x90, 0, 5]);
        assert_eq!(led_message(GridDevice::ApcMini, b, Led::Off), [0x90, 0, 0]);
        // APC Mini MK2: palette velocity, the channel its brightness or blink.
        assert_eq!(led_message(GridDevice::ApcMiniMk2, b, Led::Playing(0x0000ff)), [0x96, 0, 45]);
        assert_eq!(led_message(GridDevice::ApcMiniMk2, b, Led::Queued(0x0000ff)), [0x9E, 0, 45]);
        // X-Y Launchpad: full green, steady.
        assert_eq!(led_message(GridDevice::LaunchpadXy, GridButton::Clip { col: 1, row: 0 }, Led::Playing(0x00ff00)), [0x90, 1, 48 + 12]);
        // Generic grids have no LEDs to light.
        assert!(led_message(GridDevice::Generic, b, Led::Playing(0xffffff)).is_empty());
    }

    #[test]
    fn palette_follows_the_hue() {
        assert_eq!(palette_index(0xff0000, false), 5);
        assert_eq!(palette_index(0x800000, false), 5, "dark red is still red");
        assert_eq!(palette_index(0x3fd45a, false), 25);
        assert_eq!(palette_index(0x3ccfc4, false), 33);
        assert_eq!(palette_index(0xf08a5d, false), 9);
        assert_eq!(palette_index(0xffffff, false), 3);
        assert_eq!(palette_index(0xff0000, true), 7);
    }
}
