fn main() {
    #[cfg(target_os = "macos")]
    {
        let out = std::env::var("OUT_DIR").expect("OUT_DIR missing");
        let status = std::process::Command::new("xcrun")
            .args(["swiftc", "native/macos_audio_capture.swift", "-o"])
            .arg(format!("{out}/redkey-audio-capture"))
            .args(["-framework", "AVFoundation"])
            .status()
            .expect("failed to compile macOS audio helper");
        assert!(status.success(), "failed to compile macOS audio helper");
        println!("cargo:rerun-if-changed=native/macos_audio_capture.swift");
        println!("cargo:rustc-env=REDKEY_AUDIO_HELPER={out}/redkey-audio-capture");

        let status = std::process::Command::new("xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-c",
                "native/macos_microphone_permission.m",
                "-o",
            ])
            .arg(format!("{out}/macos_microphone_permission.o"))
            .status()
            .expect("failed to compile macOS microphone permission helper");
        assert!(
            status.success(),
            "failed to compile macOS microphone permission helper"
        );
        let status = std::process::Command::new("xcrun")
            .args(["ar", "rcs"])
            .arg(format!("{out}/libredkey_microphone_permission.a"))
            .arg(format!("{out}/macos_microphone_permission.o"))
            .status()
            .expect("failed to archive macOS microphone permission helper");
        assert!(
            status.success(),
            "failed to archive macOS microphone permission helper"
        );
        println!("cargo:rerun-if-changed=native/macos_microphone_permission.m");
        println!("cargo:rustc-link-search=native={out}");
        println!("cargo:rustc-link-lib=static=redkey_microphone_permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=dylib=objc");
    }
    tauri_build::build()
}
