#[path = "../build-support/version.rs"]
mod version;

fn main() {
    let version = version::configure();
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() == "windows" {
        let mut res = winres::WindowsResource::new();
        // https://github.com/mxre/winres/pull/24
        // https://github.com/mxre/winres/issues/42
        #[cfg(not(target_os = "windows"))]
        if std::env::var("CARGO_CFG_TARGET_ENV").unwrap().as_str() == "gnu" {
            res.set_ar_path("x86_64-w64-mingw32-ar");
            res.set_windres_path("x86_64-w64-mingw32-windres");
        }
        res.set_icon("../assets/icon.ico");

        // The shipped application must request elevation for input simulation.
        // Pure Rust test executables do not perform those actions, and Windows
        // otherwise refuses to launch them in an unelevated development shell.
        // Keep the opt-out debug-only and explicit so release artifacts can
        // never accidentally lose the administrator manifest.
        let test_as_invoker = std::env::var("PROFILE").as_deref() == Ok("debug")
            && std::env::var_os("CARGO_FEATURE_TEST_AS_INVOKER").is_some();
        if !test_as_invoker {
            res.set_manifest_file("../assets/manifest.xml");
        }

        // VS_VERSION_INFO — legitimate apps carry version metadata; its absence
        // is a negative signal for AV heuristics.
        let is_capture_scanner = std::env::var_os("CARGO_FEATURE_CAPTURE").is_some();
        let product_name = if is_capture_scanner {
            "GGScanner - Genshin & Star Rail"
        } else {
            "GGScannerOCR - Genshin & Star Rail"
        };
        let original_filename = if is_capture_scanner {
            "GGScanner.exe"
        } else {
            "GGScannerOCR.exe"
        };
        res.set("ProductName", product_name);
        res.set(
            "FileDescription",
            "Genshin Impact and Honkai: Star Rail data scanner",
        );
        res.set("ProductVersion", &version);
        res.set("FileVersion", &version);
        res.set("LegalCopyright", "GPL-2.0-or-later");
        res.set("OriginalFilename", original_filename);

        res.compile().unwrap();
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            // GNU ld can discard winres's resource-only archive because it
            // exports no referenced symbol. Link the generated COFF object
            // directly so the administrator manifest and icon really ship.
            let resource =
                std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("resource.o");
            println!("cargo:rustc-link-arg-bins={}", resource.display());
        }
    }
}
