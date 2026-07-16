use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(ffmpeg_old_channel_layout)");
    println!("cargo:rustc-check-cfg=cfg(ffmpeg_codecpar_has_framerate)");

    if pkg_config_major("libavutil").is_some_and(|major| major < 59) {
        println!("cargo:rustc-cfg=ffmpeg_old_channel_layout");
    }
    // libavcodec 62 (FFmpeg 8) inserted `framerate` between
    // `sample_aspect_ratio` and `field_order` in AVCodecParameters. Keep the
    // hand-written ABI binding aligned with the headers selected by pkg-config.
    if pkg_config_major("libavcodec").is_some_and(|major| major >= 62) {
        println!("cargo:rustc-cfg=ffmpeg_codecpar_has_framerate");
    }

    if emit_pkg_config_libs() {
        return;
    }

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("apple-darwin") {
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
        println!("cargo:rustc-link-search=native=/usr/local/lib");
    }

    for library in ["avcodec", "avformat", "avutil"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn emit_pkg_config_libs() -> bool {
    let output = Command::new("pkg-config")
        .args(["--libs", "libavformat", "libavcodec", "libavutil"])
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    for argument in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        if let Some(path) = argument.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(name) = argument.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={name}");
        }
    }

    true
}

fn pkg_config_major(library: &str) -> Option<u32> {
    let output = Command::new("pkg-config")
        .args(["--modversion", library])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .split('.')
        .next()?
        .trim()
        .parse()
        .ok()
}
