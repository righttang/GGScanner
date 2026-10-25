//! Re-read failed artifact dumps using the production OCR and parser.
use anyhow::{bail, Context, Result};
use genshin_scanner::cli::load_config_or_default;
use genshin_scanner::scanner::artifact::GoodArtifactScanner;
use genshin_scanner::scanner::common::capture_frame::CaptureFrame;
use genshin_scanner::scanner::common::coord_scaler::CoordScaler;
use genshin_scanner::scanner::common::{mappings::MappingManager, ocr_factory, pixel_profile};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().context("artifact dump directory required")?);
    let output = PathBuf::from(args.next().context("output JSON path required")?);
    let config = load_config_or_default();
    pixel_profile::set_hdr_mode(config.hdr_mode);
    let mappings = MappingManager::new(&config.to_overrides())?;
    let ocr = ocr_factory::create_ocr_model("ppocrv6tiny")?;
    let scaler = CoordScaler::new(1920, 1080);
    let mut recovered = Vec::new();
    let mut directories: Vec<_> = std::fs::read_dir(directory)?.collect::<std::io::Result<_>>()?;
    directories.sort_by_key(|entry| entry.file_name());
    for entry in directories {
        let path = entry.path();
        let meta = path.join("ocr_fields.json");
        if !meta.is_file() {
            continue;
        }
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(meta)?)?;
        let error = meta["error"].as_str().unwrap_or_default();
        if error != "cannot identify artifact level" && !error.contains("* variant of ") {
            continue;
        }
        let index = meta["index"].as_u64().context("missing scan index")? as usize;
        let image = image::open(path.join("full.png"))?.to_rgb8();
        // The production scanner saves either a full window or its anchored
        // 480x860 detail crop. Reconstruct the same window coordinate space
        // for the shared panel icon readers as well as OCR.
        let image = match image.dimensions() {
            (1920, 1080) => image,
            (480, 860) => {
                let mut full = image::RgbImage::new(1920, 1080);
                image::imageops::replace(&mut full, &image, 1310, 110);
                full
            },
            size => bail!("unsupported dump dimensions {size:?}: {}", path.display()),
        };
        let artifact = GoodArtifactScanner::identify_artifact_at(
            ocr.as_ref(),
            ocr.as_ref(),
            &CaptureFrame::full(image),
            &scaler,
            &mappings,
            index,
            None,
        )?
        .with_context(|| format!("artifact {index} still unreadable"))?;
        println!(
            "recovered {index}: {} +{} crafted={}",
            artifact.set_key, artifact.level, artifact.elixir_crafted
        );
        recovered.push(serde_json::json!({"index":index,"artifact":artifact}));
    }
    std::fs::write(output, serde_json::to_vec_pretty(&recovered)?)?;
    Ok(())
}
