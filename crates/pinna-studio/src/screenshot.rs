//! Built with `--features screenshot` and run with `PINNA_SCREENSHOT=out.png`, the
//! window saves itself once the project is loaded and settled, then quits. Agents
//! review the UI this way. `PINNA_SCREENSHOT_B=1` shows it hearing B,
//! `PINNA_SCREENSHOT_PLAY=1` plays for two seconds first, `PINNA_SCREENSHOT_SIZE=900x560`
//! sets the window size.

use std::path::PathBuf;

use eframe::egui::{self, ColorImage, ScreenshotCallback, ViewportCommand};

pub fn size() -> Option<[f32; 2]> {
    let v = std::env::var("PINNA_SCREENSHOT_SIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

pub struct Shot {
    path: Option<PathBuf>,
    hear_b: bool,
    play: bool,
    settled: u32,
    sent: bool,
}

impl Shot {
    pub fn from_env() -> Self {
        Self {
            path: std::env::var_os("PINNA_SCREENSHOT").map(PathBuf::from),
            hear_b: std::env::var_os("PINNA_SCREENSHOT_B").is_some(),
            play: std::env::var_os("PINNA_SCREENSHOT_PLAY").is_some(),
            settled: 0,
            sent: false,
        }
    }

    /// Call every frame. Returns true when playback should start.
    pub fn frame(&mut self, ctx: &egui::Context, settled: bool, hear_b: &mut bool) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        if self.hear_b {
            *hear_b = true;
        }
        ctx.request_repaint();
        self.settled = if settled { self.settled + 1 } else { 0 };
        let start = self.play && self.settled == 10;
        let at = if self.play { 130 } else { 10 };
        if self.settled >= at && !self.sent {
            self.sent = true;
            let path = path.clone();
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(ScreenshotCallback::new(
                move |img| {
                    if let Err(e) = save(&path, &img) {
                        eprintln!("screenshot: {e}");
                    }
                    std::process::exit(0);
                },
            )));
        }
        start
    }
}

fn save(path: &PathBuf, img: &ColorImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let [w, h] = img.size;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&rgba))
        .map_err(|e| e.to_string())
}
