//! Calendar anchor retained by the stock game clock, used only by the optional extension.

impl super::GameTime {
    pub(crate) fn pfui_local_epoch(&self) -> Option<f64> {
        let epoch = manila_pfui::clock::epoch(self.base_day, self.base_minute)?;
        Some(epoch + self.received.elapsed().as_secs_f64() * f64::from(self.timescale) * 60.0)
    }
}
