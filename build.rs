use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Enable platform-specific features based on target OS
    match target_os.as_str() {
        "linux" => {
            println!("cargo:rustc-cfg=feature=\"storage-linux\"");
            println!("cargo:rustc-cfg=platform_linux");
        }
        "macos" => {
            println!("cargo:rustc-cfg=feature=\"storage-macos\"");
            println!("cargo:rustc-cfg=platform_macos");
        }
        "windows" => {
            println!("cargo:rustc-cfg=feature=\"storage-windows\"");
            println!("cargo:rustc-cfg=platform_windows");
        }
        _ => {
            println!("cargo:rustc-cfg=feature=\"storage-unknown\"");
        }
    }

    // Print platform info for debugging
    println!("cargo:warning=Building for platform: {}", target_os);
}
