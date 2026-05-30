//! Mapping between iced input and Ruffle input.

use iced::{keyboard, Point, Size};

use ruffle_core::events::{KeyDescriptor, KeyLocation, LogicalKey, NamedKey, PhysicalKey};

/// Map a cursor position (relative to the widget) into the target pixel space
/// (`target` = the render-target size, which Ruffle's viewport lives in),
/// accounting for the `Contain` letterbox the shader applies.
pub fn map_cursor(area: Size, target: (u32, u32), p: Point) -> (f64, f64) {
    let area_w = area.width.max(1.0) as f64;
    let area_h = area.height.max(1.0) as f64;
    let (sw, sh) = (target.0 as f64, target.1 as f64);
    let scale = (area_w / sw).min(area_h / sh);
    let (disp_w, disp_h) = (sw * scale, sh * scale);
    let (off_x, off_y) = ((area_w - disp_w) / 2.0, (area_h - disp_h) / 2.0);
    let x = ((p.x as f64 - off_x) / scale).clamp(0.0, sw);
    let y = ((p.y as f64 - off_y) / scale).clamp(0.0, sh);
    (x, y)
}

/// Translate an iced key into a Ruffle `KeyDescriptor`, synthesizing the physical
/// key from the logical one for the common set (letters, digits, arrows, etc.).
pub fn to_key_descriptor(
    key: &keyboard::Key,
    location: keyboard::Location,
) -> Option<KeyDescriptor> {
    use keyboard::key::Named;

    let key_location = match location {
        keyboard::Location::Standard => KeyLocation::Standard,
        keyboard::Location::Left => KeyLocation::Left,
        keyboard::Location::Right => KeyLocation::Right,
        keyboard::Location::Numpad => KeyLocation::Numpad,
    };

    let (physical_key, logical_key) = match key {
        keyboard::Key::Character(s) => {
            let c = s.chars().next()?;
            let physical = match c.to_ascii_lowercase() {
                'a' => PhysicalKey::KeyA,
                'b' => PhysicalKey::KeyB,
                'c' => PhysicalKey::KeyC,
                'd' => PhysicalKey::KeyD,
                'e' => PhysicalKey::KeyE,
                'f' => PhysicalKey::KeyF,
                'g' => PhysicalKey::KeyG,
                'h' => PhysicalKey::KeyH,
                'i' => PhysicalKey::KeyI,
                'j' => PhysicalKey::KeyJ,
                'k' => PhysicalKey::KeyK,
                'l' => PhysicalKey::KeyL,
                'm' => PhysicalKey::KeyM,
                'n' => PhysicalKey::KeyN,
                'o' => PhysicalKey::KeyO,
                'p' => PhysicalKey::KeyP,
                'q' => PhysicalKey::KeyQ,
                'r' => PhysicalKey::KeyR,
                's' => PhysicalKey::KeyS,
                't' => PhysicalKey::KeyT,
                'u' => PhysicalKey::KeyU,
                'v' => PhysicalKey::KeyV,
                'w' => PhysicalKey::KeyW,
                'x' => PhysicalKey::KeyX,
                'y' => PhysicalKey::KeyY,
                'z' => PhysicalKey::KeyZ,
                '0' => PhysicalKey::Digit0,
                '1' => PhysicalKey::Digit1,
                '2' => PhysicalKey::Digit2,
                '3' => PhysicalKey::Digit3,
                '4' => PhysicalKey::Digit4,
                '5' => PhysicalKey::Digit5,
                '6' => PhysicalKey::Digit6,
                '7' => PhysicalKey::Digit7,
                '8' => PhysicalKey::Digit8,
                '9' => PhysicalKey::Digit9,
                _ => PhysicalKey::Unknown,
            };
            (physical, LogicalKey::Character(c))
        }
        keyboard::Key::Named(named) => match named {
            Named::ArrowUp => (PhysicalKey::ArrowUp, LogicalKey::Named(NamedKey::ArrowUp)),
            Named::ArrowDown => (
                PhysicalKey::ArrowDown,
                LogicalKey::Named(NamedKey::ArrowDown),
            ),
            Named::ArrowLeft => (
                PhysicalKey::ArrowLeft,
                LogicalKey::Named(NamedKey::ArrowLeft),
            ),
            Named::ArrowRight => (
                PhysicalKey::ArrowRight,
                LogicalKey::Named(NamedKey::ArrowRight),
            ),
            Named::Space => (PhysicalKey::Space, LogicalKey::Character(' ')),
            Named::Enter => (PhysicalKey::Enter, LogicalKey::Named(NamedKey::Enter)),
            Named::Backspace => (
                PhysicalKey::Backspace,
                LogicalKey::Named(NamedKey::Backspace),
            ),
            Named::Tab => (PhysicalKey::Tab, LogicalKey::Named(NamedKey::Tab)),
            Named::Escape => (PhysicalKey::Escape, LogicalKey::Named(NamedKey::Escape)),
            Named::Delete => (PhysicalKey::Delete, LogicalKey::Named(NamedKey::Delete)),
            Named::Shift => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::ShiftRight
                } else {
                    PhysicalKey::ShiftLeft
                };
                (p, LogicalKey::Named(NamedKey::Shift))
            }
            Named::Control => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::ControlRight
                } else {
                    PhysicalKey::ControlLeft
                };
                (p, LogicalKey::Named(NamedKey::Control))
            }
            Named::Alt => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::AltRight
                } else {
                    PhysicalKey::AltLeft
                };
                (p, LogicalKey::Named(NamedKey::Alt))
            }
            _ => return None,
        },
        _ => return None,
    };

    Some(KeyDescriptor {
        physical_key,
        logical_key,
        key_location,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: (u32, u32) = (100, 100);

    fn approx(a: (f64, f64), b: (f64, f64)) {
        assert!(
            (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn maps_center_to_target_center() {
        // Widget matches the target exactly: 1:1, no letterbox.
        let area = Size::new(100.0, 100.0);
        approx(
            map_cursor(area, TARGET, Point::new(50.0, 50.0)),
            (50.0, 50.0),
        );
        approx(map_cursor(area, TARGET, Point::new(0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn divides_out_uniform_scale() {
        // Widget is 2x the target on both axes: positions scale back down.
        let area = Size::new(200.0, 200.0);
        approx(
            map_cursor(area, TARGET, Point::new(100.0, 100.0)),
            (50.0, 50.0),
        );
    }

    #[test]
    fn accounts_for_letterbox_offset() {
        // Wide widget: target is centered horizontally with bars on the sides.
        // scale = 1, horizontal offset = (200 - 100) / 2 = 50.
        let area = Size::new(200.0, 100.0);
        // Widget center -> target center.
        approx(
            map_cursor(area, TARGET, Point::new(100.0, 50.0)),
            (50.0, 50.0),
        );
        // Left edge of the content area -> target x = 0.
        approx(map_cursor(area, TARGET, Point::new(50.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn clamps_into_the_letterbox_bars() {
        let area = Size::new(200.0, 100.0);
        // A point inside the left bar (x < 50) clamps to the target's left edge.
        approx(
            map_cursor(area, TARGET, Point::new(10.0, 50.0)),
            (0.0, 50.0),
        );
        // Far beyond the bottom-right clamps to the target's max corner.
        approx(
            map_cursor(area, TARGET, Point::new(9999.0, 9999.0)),
            (100.0, 100.0),
        );
    }
}
