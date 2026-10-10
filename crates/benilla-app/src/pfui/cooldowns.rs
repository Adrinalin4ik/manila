//! The optional Nampower clock reads the GCD leg, never the longest spell cooldown.

use bevy::platform::time::Instant;

impl super::Cooldowns {
    pub(crate) fn pfui_gcd_remaining_ms(&self, category: u32) -> f64 {
        let now = Instant::now();
        self.records
            .iter()
            .filter(|record| record.gcd_category == category)
            .map(|record| record.gcd.remaining(now).as_secs_f64() * 1000.0)
            .fold(0.0, f64::max)
    }

    pub(crate) fn pfui_gcd_end(&self, category: u32, clock: &crate::ui_script::UiClock) -> f64 {
        let now = Instant::now();
        self.records
            .iter()
            .filter(|record| {
                record.gcd_category == category && !record.gcd.remaining(now).is_zero()
            })
            .map(|record| record.gcd)
            .max_by_key(|timer| timer.remaining(now))
            .map_or(0.0, |timer| {
                let end = timer.start + timer.duration;
                let offset = if end >= clock.anchor {
                    end.duration_since(clock.anchor).as_secs_f64()
                } else {
                    -clock.anchor.duration_since(end).as_secs_f64()
                };
                clock.ui_now + offset
            })
    }
}
