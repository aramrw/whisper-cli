use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/macos_audio.m");

        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
        let obj_path = out_dir.join("macos_audio.o");
        let lib_path = out_dir.join("libmacos_audio.a");

        let status = Command::new("clang")
            .args([
                "-c",
                "-O2",
                "-fobjc-arc",
                "-Wall",
                "-Wextra",
                "-Wno-unused-parameter",
                "src/macos_audio.m",
                "-o",
            ])
            .arg(&obj_path)
            .status()
            .expect("Failed to execute clang to compile src/macos_audio.m");

        assert!(status.success(), "clang failed to compile src/macos_audio.m");

        let ar_status = Command::new("ar")
            .args(["crus"])
            .arg(&lib_path)
            .arg(&obj_path)
            .status()
            .expect("Failed to execute ar to archive libmacos_audio.a");

        assert!(ar_status.success(), "ar failed to create libmacos_audio.a");

        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=macos_audio");
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
        println!("cargo:rustc-link-lib=framework=SoundAnalysis");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
