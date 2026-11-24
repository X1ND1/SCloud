//! A thin wrapper over libmpv: mpv handles progressive MP3 and HLS by itself.

use libmpv2::Mpv;

pub struct Player {
    mpv: Mpv,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let mpv = Mpv::new().map_err(|e| format!("libmpv: {e:?}"))?;
        // audio only, no video window; at the end of a track don't quit, set eof-reached instead
        for (k, v) in [
            ("vid", "no"),
            ("video", "no"),
            ("ytdl", "no"),
            ("keep-open", "yes"),
            // changing the speed changes the pitch too, which is how "slowed" / "nightcore" sound
            ("audio-pitch-correction", "no"),
            // by default mpv buffers up to ~150 MB of stream; a couple of megabytes is enough for music
            ("cache", "yes"),
            ("demuxer-max-bytes", "6MiB"),
            ("demuxer-max-back-bytes", "2MiB"),
        ] {
            let _ = mpv.set_property(k, v);
        }
        let _ = mpv.set_property("volume", 80i64);
        Ok(Self { mpv })
    }

    pub fn load(&self, url: &str) {
        let _ = self.mpv.command("loadfile", &[url]);
        let _ = self.mpv.set_property("pause", false);
    }

    pub fn resume(&self) {
        let _ = self.mpv.set_property("pause", false);
    }

    pub fn toggle_pause(&self) {
        let paused = self.is_paused();
        let _ = self.mpv.set_property("pause", !paused);
    }

    pub fn is_paused(&self) -> bool {
        self.mpv.get_property::<bool>("pause").unwrap_or(true)
    }

    pub fn position(&self) -> f64 {
        self.mpv.get_property::<f64>("time-pos").unwrap_or(0.0)
    }

    pub fn duration(&self) -> f64 {
        self.mpv.get_property::<f64>("duration").unwrap_or(0.0)
    }

    pub fn at_end(&self) -> bool {
        self.mpv.get_property::<bool>("eof-reached").unwrap_or(false)
    }

    pub fn seek(&self, secs: f64) {
        let pos = format!("{secs:.2}");
        let _ = self.mpv.command("seek", &[pos.as_str(), "absolute"]);
    }

    pub fn set_volume(&self, vol: f32) {
        let _ = self.mpv.set_property("volume", vol as f64);
    }

    pub fn stop(&self) {
        let _ = self.mpv.command("stop", &[]);
    }

    pub fn set_speed(&self, speed: f64) {
        let _ = self.mpv.set_property("speed", speed);
    }

    /// mpv audio filter chain (lavfi); an empty string means no effects.
    pub fn set_af(&self, af: &str) {
        let _ = self.mpv.set_property("af", af);
    }
}
