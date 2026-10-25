//! Save diagnostic frames through the same capture implementations as the scanner.
use anyhow::Result;
use genshin_scanner::cli::GoodScannerApplication;
use yas::capture::{create_capturer, CaptureMethod};

fn main() -> Result<()> {
    let game = GoodScannerApplication::get_game_info()?;
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "capture_probe".into());
    std::fs::create_dir_all(&output)?;
    for (name, method, hdr) in [
        ("bitblt", CaptureMethod::BitBlt, false),
        ("wgc_sdr", CaptureMethod::Wgc, false),
        ("wgc_hdr", CaptureMethod::Wgc, true),
    ] {
        let capturer = create_capturer(game.native_window_handle(), method, hdr, 4.0)?;
        let image = capturer.capture_rect(game.window)?;
        image.save(std::path::Path::new(&output).join(format!("{name}.png")))?;
        println!("{name}: {}x{}", image.width(), image.height());
    }
    Ok(())
}
