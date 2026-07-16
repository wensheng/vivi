mod cli;
mod client;
mod ffmpeg;
mod image_viewer;
mod terminal_geometry;
mod video_player;

pub use vivid_protocol as protocol;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Config;
use crate::client::VividClient;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vivi: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse();
    config.validate()?;

    let mut client = VividClient::connect(&config)?;
    for file in &config.files {
        let result = if looks_like_video(file) {
            video_player::play(&config, &mut client, file)
        } else {
            match image_viewer::view(&config, &mut client, file) {
                Ok(()) => Ok(()),
                Err(image_error) => match video_player::play(&config, &mut client, file) {
                    Ok(()) => Ok(()),
                    Err(video_error) => Err(std::io::Error::other(format!(
                        "could not read {} as an image ({image_error}) or video ({video_error})",
                        file.display()
                    ))
                    .into()),
                },
            }
        };

        result?;
    }

    client.goodbye()?;
    Ok(())
}

fn looks_like_video(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "3gp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_video_extensions_case_insensitively() {
        assert!(looks_like_video(Path::new("clip.MKV")));
        assert!(looks_like_video(Path::new("clip.mp4")));
        assert!(!looks_like_video(Path::new("photo.png")));
    }
}
