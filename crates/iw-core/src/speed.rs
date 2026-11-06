//! How fast the cursor moves on another device.
//!
//! The controller's user can make the cursor move faster or slower on each
//! remote device than the mouse moves it here, as for a large screen or one
//! with a high resolution. The router scales the part of each ordinary
//! pointer movement that the cursor makes on a remote device by that
//! device's speed (`Layout::step_at`): a movement that crosses an edge goes
//! at the speed of the device it starts on until it crosses, and at the
//! speed of the device it enters after. On this machine the cursor always
//! moves as the mouse does, and relative movements, which games scale
//! themselves, are never scaled.

use crate::geometry::Point;

/// The cursor's speed on a device, as a percentage of how the mouse moves
/// it here: 50 to 300, in steps of 10.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CursorSpeed(u16);

impl CursorSpeed {
    /// As fast as here.
    pub const NORMAL: Self = Self(100);
    pub const MIN_PERCENT: u32 = 50;
    pub const MAX_PERCENT: u32 = 300;
    pub const STEP_PERCENT: u32 = 10;

    /// The speed of `percent`, if it is one: from `MIN_PERCENT` to
    /// `MAX_PERCENT`, a multiple of `STEP_PERCENT`.
    pub fn from_percent(percent: u32) -> Option<Self> {
        let valid = (Self::MIN_PERCENT..=Self::MAX_PERCENT).contains(&percent)
            && percent.is_multiple_of(Self::STEP_PERCENT);
        if !valid {
            return None;
        }
        u16::try_from(percent).ok().map(Self)
    }

    pub fn percent(self) -> u32 {
        u32::from(self.0)
    }

    /// How many times as far the cursor moves there as the mouse moves it
    /// here.
    pub fn factor(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// `delta`, a movement of the mouse here, as the cursor moves there.
    pub fn apply(self, delta: Point) -> Point {
        delta.scaled(self.factor())
    }
}

impl Default for CursorSpeed {
    fn default() -> Self {
        Self::NORMAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_go_from_half_to_three_times_in_tens() {
        for percent in [50, 60, 100, 150, 290, 300] {
            assert_eq!(
                CursorSpeed::from_percent(percent).map(CursorSpeed::percent),
                Some(percent)
            );
        }
        for percent in [0, 40, 49, 55, 101, 155, 301, 310, 1000, u32::MAX] {
            assert_eq!(CursorSpeed::from_percent(percent), None, "{percent}");
        }
        assert_eq!(CursorSpeed::default(), CursorSpeed::NORMAL);
        assert_eq!(CursorSpeed::NORMAL.percent(), 100);
    }

    #[test]
    fn a_speed_scales_both_directions_alike() {
        let speed = |p| CursorSpeed::from_percent(p).unwrap();
        assert_eq!(
            speed(150).apply(Point::new(10.0, -4.0)),
            Point::new(15.0, -6.0)
        );
        assert_eq!(speed(50).apply(Point::new(10.0, 3.0)), Point::new(5.0, 1.5));
        assert_eq!(
            CursorSpeed::NORMAL.apply(Point::new(0.3, 0.7)),
            Point::new(0.3, 0.7)
        );
    }
}
