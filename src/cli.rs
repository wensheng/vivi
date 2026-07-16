use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "vivi",
    version,
    about = "Display images and play videos in Vivido using the Vivid Protocol",
    long_about = "A Vivid Protocol image viewer and video player. It connects to the private \
                  per-window endpoint inherited from Vivido; --dry-run and --trace-dir generate \
                  deterministic wire fixtures without a presenter."
)]
pub struct Config {
    /// Image or video files to display.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Zoom multiplier applied to the media's natural pixel size.
    #[arg(short = 'z', long, default_value_t = 1.0)]
    pub zoom: f32,

    /// Vivid endpoint, normally inherited from Vivido as VIVID_ENDPOINT.
    #[arg(long, env = "VIVID_ENDPOINT")]
    pub endpoint: Option<String>,

    /// Vivid capability token, normally inherited from Vivido as VIVID_TOKEN.
    #[arg(long, env = "VIVID_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Build the complete request stream without connecting to Vivido.
    #[arg(long)]
    pub dry_run: bool,

    /// Write each Vivid connection to a separate file in this directory.
    /// Implies --dry-run.
    #[arg(long, value_name = "DIRECTORY")]
    pub trace_dir: Option<PathBuf>,

    /// Print source, placement, packet, and protocol progress.
    #[arg(short, long)]
    pub verbose: bool,

    /// Exit as soon as media has been submitted instead of waiting for video playback.
    #[arg(long)]
    pub no_wait: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.zoom.is_finite() || self.zoom <= 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "--zoom must be a finite number greater than zero",
            )
            .into());
        }

        if !self.is_dry_run() && self.endpoint.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "VIVID_ENDPOINT is not set; run Vivi inside Vivido or use --dry-run \
                 (optionally with --trace-dir)",
            )
            .into());
        }

        if !self.is_dry_run() && self.token.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "VIVID_TOKEN is not set",
            )
            .into());
        }

        Ok(())
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run || self.trace_dir.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            files: vec![PathBuf::from("image.png")],
            zoom: 1.0,
            endpoint: None,
            token: None,
            dry_run: true,
            trace_dir: None,
            verbose: false,
            no_wait: false,
        }
    }

    #[test]
    fn dry_run_does_not_require_endpoint_or_token() {
        assert!(config().validate().is_ok());
    }

    #[test]
    fn rejects_invalid_zoom() {
        let mut config = config();
        config.zoom = f32::NAN;
        assert!(config.validate().is_err());
    }
}
