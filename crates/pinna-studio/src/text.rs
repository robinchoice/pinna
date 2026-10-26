//! German and English UI text. Measurements and code names stay as Pinna writes them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    De,
    En,
}

impl Lang {
    /// German unless the system locale says otherwise.
    pub fn from_env() -> Self {
        let l = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        if l.is_empty() || l.starts_with("de") {
            Self::De
        } else {
            Self::En
        }
    }

    pub fn t(self, de: &'static str, en: &'static str) -> &'static str {
        match self {
            Self::De => de,
            Self::En => en,
        }
    }

    /// The language an agent should write proposals in.
    pub fn name(self) -> &'static str {
        self.t("German", "English")
    }
}

/// How a metric reads better: lower, higher, or neither.
#[derive(Clone, Copy, PartialEq)]
pub enum Better {
    Lower,
    Higher,
    Neither,
}

/// Display name of an evidence metric (see `pinna::measure`), and which way is better.
pub fn metric(lang: Lang, name: &str) -> (String, Better) {
    let (de, en, better) = match name {
        "integrated LUFS" => ("Lautheit, LUFS", "Loudness, LUFS", Better::Neither),
        "short-term max LUFS" => (
            "Kurzzeit max., LUFS",
            "Short-term max, LUFS",
            Better::Neither,
        ),
        "true peak dBTP" => ("True Peak, dBTP", "True peak, dBTP", Better::Neither),
        "LRA LU" => ("Lautheitsumfang, LU", "Loudness range, LU", Better::Neither),
        "kick punch dB" => ("Kick-Transiente, dB", "Kick punch, dB", Better::Higher),
        "correlation" => ("Phasenkorrelation", "Correlation", Better::Neither),
        "width" => ("Stereobreite", "Stereo width", Better::Neither),
        "mono loss LU" => ("Mono-Verlust, LU", "Mono loss, LU", Better::Lower),
        "masking worst band" => ("Stärkste Maskierung", "Worst masking band", Better::Neither),
        n if n.starts_with("masking index") => {
            let pair = n.trim_start_matches("masking index").trim();
            return (
                format!("{} {pair}", lang.t("Maskierung", "Masking")),
                Better::Lower,
            );
        }
        n => return (n.to_string(), Better::Neither),
    };
    (lang.t(de, en).to_string(), better)
}
