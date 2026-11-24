//! Effects: slowed / nightcore, pitch, reverb, bass boost, 8D, lo-fi, "underwater", phone,
//! chorus, echo, karaoke… All of them are ffmpeg filters inside mpv (and the same ones for MP3 downloads).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fx {
    /// 1.0 = original, lower = slowed, higher = nightcore (speed together with pitch)
    pub speed: f32,
    /// pitch shift in semitones without changing speed, −6..+6
    pub pitch: f32,
    /// 0..1
    pub reverb: f32,
    /// bass boost in dB, 0..15
    pub bass: f32,
    /// treble in dB, −10..+10
    pub treble: f32,
    /// "muffle" / "underwater": 0..1
    pub muffle: f32,
    /// the sound "circles" around your head
    pub eight_d: bool,
    /// vinyl lo-fi: cut highs + light "grain"
    pub lofi: bool,
    /// as if playing from a phone/walkie-talkie
    pub phone: bool,
    pub chorus: bool,
    pub echo: bool,
    /// remove vocals (karaoke): subtract the stereo center
    pub karaoke: bool,
    /// equalizer: 10 bands (31 Hz … 16 kHz), dB −12..+12
    pub eq: [f32; 10],
    /// loudness normalization between quiet and loud tracks
    pub normalize: bool,
}

/// Equalizer band frequencies.
pub const EQ_BANDS: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

impl Default for Fx {
    fn default() -> Self {
        Self::ORIGINAL
    }
}

const fn fx() -> Fx {
    Fx {
        speed: 1.0,
        pitch: 0.0,
        reverb: 0.0,
        bass: 0.0,
        treble: 0.0,
        muffle: 0.0,
        eight_d: false,
        lofi: false,
        phone: false,
        chorus: false,
        echo: false,
        karaoke: false,
        eq: [0.0; 10],
        normalize: false,
    }
}

impl Fx {
    pub const ORIGINAL: Fx = fx();

    pub const PRESETS: [(&'static str, Fx); 13] = [
        ("Оригинал", fx()),
        (
            "Slowed + Reverb",
            Fx {
                speed: 0.85,
                reverb: 0.6,
                ..fx()
            },
        ),
        (
            "Slowed + Bass",
            Fx {
                speed: 0.85,
                reverb: 0.3,
                bass: 9.0,
                ..fx()
            },
        ),
        (
            "Nightcore",
            Fx {
                speed: 1.25,
                treble: 2.0,
                ..fx()
            },
        ),
        ("Sped Up", Fx { speed: 1.15, ..fx() }),
        ("Bass Boost", Fx { bass: 10.0, ..fx() }),
        (
            "8D",
            Fx {
                reverb: 0.35,
                eight_d: true,
                ..fx()
            },
        ),
        (
            "Vaporwave",
            Fx {
                speed: 0.78,
                reverb: 0.7,
                muffle: 0.2,
                chorus: true,
                ..fx()
            },
        ),
        (
            "Lo-fi",
            Fx {
                speed: 0.95,
                muffle: 0.35,
                lofi: true,
                ..fx()
            },
        ),
        (
            "Под водой",
            Fx {
                muffle: 0.9,
                reverb: 0.5,
                bass: 4.0,
                ..fx()
            },
        ),
        (
            "Концертный зал",
            Fx {
                reverb: 0.9,
                echo: true,
                ..fx()
            },
        ),
        ("Телефон", Fx { phone: true, ..fx() }),
        (
            "Минусовка",
            Fx {
                karaoke: true,
                ..fx()
            },
        ),
    ];

    pub fn is_active(&self) -> bool {
        *self != Fx::ORIGINAL
    }

    /// lavfi filter chain (without speed, mpv changes that itself).
    fn chain(&self, rate: u32) -> Vec<String> {
        let mut f: Vec<String> = Vec::new();
        if self.karaoke {
            // vocals usually sit in the center: L−R removes them (some bass goes too)
            f.push("pan=stereo|c0=c0-c1|c1=c1-c0".into());
        }
        if self.pitch.abs() > 0.05 {
            // pitch shift without changing tempo: change the rate and restore the tempo with atempo
            let k = 2f32.powf(self.pitch / 12.0);
            f.push(format!(
                "asetrate={rate}*{k:.4},aresample={rate},atempo={:.4}",
                1.0 / k
            ));
        }
        if self.bass > 0.5 {
            f.push(format!("bass=g={:.1}:f=100:w=0.6", self.bass));
        }
        for (freq, g) in EQ_BANDS.iter().zip(self.eq) {
            if g.abs() > 0.2 {
                f.push(format!("equalizer=f={freq}:t=o:w=1:g={g:.1}"));
            }
        }
        if self.treble.abs() > 0.5 {
            f.push(format!("treble=g={:.1}:f=4000", self.treble));
        }
        if self.muffle > 0.01 {
            // the higher, the lower the cutoff (underwater ~400 Hz)
            let cutoff = 12000.0 * (1.0 - self.muffle).powi(2) + 400.0;
            f.push(format!("lowpass=f={cutoff:.0}"));
        }
        if self.lofi {
            f.push("lowpass=f=5200,highpass=f=90,acrusher=bits=12:mix=0.25:mode=log".into());
        }
        if self.phone {
            f.push("highpass=f=400,lowpass=f=3200,acompressor=threshold=0.1:ratio=6".into());
        }
        if self.chorus {
            f.push("chorus=0.6:0.9:50|60|40:0.4|0.32|0.3:0.25|0.4|0.3:2|2.3|1.3".into());
        }
        if self.reverb > 0.01 {
            // several decaying reflections: a simple "hall"
            let delays = [37, 71, 113, 163, 223, 307];
            let base = [0.50, 0.42, 0.35, 0.28, 0.21, 0.15];
            let decays: Vec<String> = base.iter().map(|d| format!("{:.3}", d * self.reverb)).collect();
            let delays: Vec<String> = delays.iter().map(|d| d.to_string()).collect();
            f.push(format!(
                "aecho=0.85:0.8:{}:{}",
                delays.join("|"),
                decays.join("|")
            ));
        }
        if self.echo {
            f.push("aecho=0.9:0.7:420|840:0.35|0.18".into());
        }
        if self.eight_d {
            f.push("apulsator=hz=0.08:amount=0.9".into());
        }
        if self.normalize {
            f.push("dynaudnorm=f=200:g=15".into());
        }
        if !f.is_empty() {
            f.push("alimiter=limit=0.95".into()); // to avoid clipping
        }
        f
    }

    /// String for mpv's `af` property.
    pub fn af(&self) -> String {
        let f = self.chain(44100);
        if f.is_empty() {
            String::new()
        } else {
            format!("lavfi=[{}]", f.join(","))
        }
    }

    /// The same chain for ffmpeg when downloading. Speed goes through asetrate (which also changes pitch).
    pub fn ffmpeg_filter(&self) -> Option<String> {
        let mut f = Vec::new();
        if (self.speed - 1.0).abs() > 0.005 {
            f.push(format!("asetrate=44100*{:.3},aresample=44100", self.speed));
        }
        f.extend(self.chain(44100));
        (!f.is_empty()).then(|| f.join(","))
    }

    /// Label for Discord and the player: "slowed + reverb".
    pub fn label(&self) -> Option<String> {
        if let Some((name, _)) = Self::PRESETS.iter().skip(1).find(|(_, p)| p == self) {
            return Some(crate::i18n::t(name).to_lowercase());
        }
        let mut p: Vec<String> = Vec::new();
        if self.speed < 0.97 {
            p.push("slowed".into());
        } else if self.speed > 1.03 {
            p.push("sped up".into());
        }
        if self.pitch.abs() > 0.05 {
            p.push(format!("pitch {:+.0}", self.pitch));
        }
        for (on, name) in [
            (self.reverb > 0.01, "reverb"),
            (self.bass > 0.5, "bass boosted"),
            (self.muffle > 0.01, "muffled"),
            (self.lofi, "lo-fi"),
            (self.phone, "phone"),
            (self.chorus, "chorus"),
            (self.echo, "echo"),
            (self.eight_d, "8D"),
            (self.karaoke, "instrumental"),
            (self.eq.iter().any(|g| g.abs() > 0.2), "EQ"),
        ] {
            if on {
                p.push(name.into());
            }
        }
        (!p.is_empty()).then(|| p.join(" + "))
    }
}
