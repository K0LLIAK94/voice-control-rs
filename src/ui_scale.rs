//! Logical layout units are converted to real device pixels, never a stretched
//! window bitmap. User zoom is relative to the monitor's Windows DPI setting.

pub const WIDTH: i32 = 720;
pub const HEIGHT: i32 = 750;
pub const MIN_WIDTH: i32 = 690;
pub const MIN_HEIGHT: i32 = 720;

#[derive(Clone, Copy, Debug)]
pub struct Scale {
    pub dpi: u32,
    pub requested: u32,
    pub effective: u32,
}

impl Scale {
    pub fn new(dpi: u32, requested: u32) -> Self {
        let requested = requested.clamp(75, 200);
        Self {
            dpi: dpi.max(1),
            requested,
            effective: requested,
        }
    }

    pub fn fit(&mut self, width: i32, height: i32) {
        let horizontal = i64::from(width.max(1)) * 9_600 / (i64::from(WIDTH) * i64::from(self.dpi));
        let vertical = i64::from(height.max(1)) * 9_600 / (i64::from(HEIGHT) * i64::from(self.dpi));
        // Automatic fit may be smaller than the manual 75% minimum on a very
        // small/high-DPI monitor. Preserve the user's requested zoom separately.
        self.effective = self.requested.min(horizontal.min(vertical).max(1) as u32);
    }

    pub fn px(self, logical: i32) -> i32 {
        let n = i64::from(logical) * i64::from(self.dpi) * i64::from(self.effective);
        ((n + n.signum() * 4_800) / 9_600) as i32
    }

    pub fn logical(self, pixels: i32) -> i32 {
        let divisor = i64::from(self.dpi) * i64::from(self.effective);
        let n = i64::from(pixels) * 9_600;
        ((n + n.signum() * divisor / 2) / divisor) as i32
    }

    pub fn label(self) -> String {
        if self.effective == self.requested {
            format!("Масштаб {}%", self.effective)
        } else {
            format!("Масштаб {}% · по размеру экрана", self.effective)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_dpi_and_user_zoom_scale_both_geometry_and_fonts() {
        assert_eq!(Scale::new(96, 100).px(WIDTH), 720);
        let scale = Scale::new(192, 125);
        assert_eq!(scale.px(WIDTH), 1800);
        assert_eq!(scale.px(20), 50);
        assert_eq!(scale.logical(1800), WIDTH);
    }

    #[test]
    fn four_k_at_two_hundred_percent_keeps_the_whole_window_visible() {
        let mut scale = Scale::new(192, 200);
        scale.fit(3840, 2080);
        assert_eq!(scale.requested, 200);
        assert!(scale.px(WIDTH) <= 3840);
        assert!(scale.px(HEIGHT) <= 2080);
        assert!(scale.effective < scale.requested);
    }

    #[test]
    fn moving_to_a_larger_screen_restores_requested_zoom() {
        let mut scale = Scale::new(192, 175);
        scale.fit(1920, 1000);
        assert!(scale.effective < 75);
        scale.fit(7680, 4240);
        assert_eq!(scale.effective, 175);
    }
}
