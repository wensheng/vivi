use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cli::Config;
use crate::client::VividClient;
use crate::ffmpeg::VideoDemuxer;
use crate::protocol::media::VideoPacket;
use crate::protocol::wire::ConnectionKind;
use crate::terminal_geometry::{TerminalGeometry, cells_for_pixels, reserve_rows};

const FIT_MARGIN_COLS: u16 = 4;
const FIT_MARGIN_ROWS: u16 = 2;
const INITIAL_BUFFER_US: u64 = 33_000;

pub fn play(
    config: &Config,
    client: &mut VividClient,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let info = VideoDemuxer::inspect(path)?;
    if info.colorimetry_inferred {
        eprintln!(
            "vivi: warning: {} does not declare complete colorimetry; using primaries={}, transfer={}, matrix={}, range={}",
            path.display(),
            info.color_primaries,
            info.transfer,
            info.matrix,
            info.range
        );
    }
    let mut demuxer = VideoDemuxer::open(path)?;
    let (columns, rows) = display_size(
        info.width,
        info.height,
        info.sar_num,
        info.sar_den,
        config.zoom,
        TerminalGeometry::current(),
    );

    let source_id = client.allocate_id()?;
    let node_id = client.allocate_id()?;
    let mut source = client.create_video_source(source_id, &info)?;
    let anchor_id = client.create_text_anchor()?;
    client.place_source(source_id, node_id, anchor_id, columns, rows)?;
    if !config.is_dry_run() {
        reserve_rows(rows)?;
    }
    let mut channel = client.open_media_channel(&source, ConnectionKind::Video)?;

    client.verbose(format_args!(
        "video {}: codec={} packetization={} {}x{} -> {columns}x{rows} cells",
        path.display(),
        info.codec,
        info.packetization,
        info.width,
        info.height
    ));

    let mut packet_id = 0_u64;
    let mut encoded_bytes = 0_u64;
    let mut playback_started = false;
    let mut playback_wall_start = None;
    let mut first_pts_us = None;
    let mut last_pts_us = None;
    let mut epoch = 1_u32;
    let mut awaiting_keyframe = false;
    while let Some(packet) = demuxer.next_packet()? {
        client.apply_pending_source_events(&mut source)?;
        if let Some(minimum_epoch) = source.take_keyframe_request() {
            epoch = epoch.saturating_add(1).max(minimum_epoch);
            awaiting_keyframe = true;
        }
        if awaiting_keyframe && !packet.key {
            continue;
        }
        if awaiting_keyframe {
            client.flush(source_id, epoch)?;
            awaiting_keyframe = false;
        }
        if !source.is_visible() {
            client.pause(source_id)?;
            client.wait_until_visible(&mut source)?;
            client.play(source_id, INITIAL_BUFFER_US)?;
        }
        if packet.data.is_empty() {
            continue;
        }
        packet_id = packet_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("video packet ID space exhausted"))?;
        encoded_bytes = encoded_bytes.saturating_add(packet.data.len() as u64);
        if packet.pts_us != i64::MIN {
            first_pts_us.get_or_insert(packet.pts_us);
            last_pts_us =
                Some(last_pts_us.map_or(packet.pts_us, |last: i64| last.max(packet.pts_us)));
        }
        client.send_video_packet(
            &mut source,
            &mut channel,
            VideoPacket {
                epoch,
                packet_id,
                pts_us: packet.pts_us,
                dts_us: packet.dts_us,
                duration_us: 0,
                key: packet.key,
                data: &packet.data,
            },
        )?;

        if !playback_started {
            client.play(source_id, INITIAL_BUFFER_US)?;
            playback_started = true;
            playback_wall_start = Some(Instant::now());
        }
        if packet_id.is_multiple_of(120) {
            client.verbose(format_args!(
                "video source {source_id}: sent {packet_id} packets / {encoded_bytes} bytes"
            ));
        }
    }

    if !playback_started {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "video has no packets").into());
    }
    client.eos(source_id, epoch)?;
    if !config.no_wait
        && let (Some(started), Some(first_pts), Some(last_pts)) =
            (playback_wall_start, first_pts_us, last_pts_us)
    {
        let timeline = Duration::from_micros(last_pts.saturating_sub(first_pts).max(0) as u64);
        let remaining = timeline.saturating_sub(started.elapsed()) + Duration::from_millis(250);
        if !remaining.is_zero() {
            client.verbose(format_args!(
                "waiting {:.2?} for presenter playback",
                remaining
            ));
            std::thread::sleep(remaining);
        }
    }
    client.verbose(format_args!(
        "video source {source_id}: EOS after {packet_id} packets / {encoded_bytes} bytes"
    ));
    Ok(())
}

fn display_size(
    width: u32,
    height: u32,
    sar_num: u32,
    sar_den: u32,
    zoom: f32,
    geometry: TerminalGeometry,
) -> (u32, u32) {
    let sample_aspect_ratio = f64::from(sar_num) / f64::from(sar_den.max(1));
    let desired_width = (width as f64 * sample_aspect_ratio * f64::from(zoom))
        .round()
        .max(1.0);
    let desired_height = (height as f64 * f64::from(zoom)).round().max(1.0);
    let maximum_width = f64::from(geometry.drawable_width_px(FIT_MARGIN_COLS));
    let maximum_height = f64::from(geometry.drawable_height_px(FIT_MARGIN_ROWS));
    let scale = (maximum_width / desired_width)
        .min(maximum_height / desired_height)
        .min(1.0);
    let target_width = (desired_width * scale).round().clamp(1.0, maximum_width) as u32;
    let target_height = (desired_height * scale).round().clamp(1.0, maximum_height) as u32;
    (
        cells_for_pixels(target_width, geometry.cell_width_px),
        cells_for_pixels(target_height, geometry.cell_height_px),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_fit_preserves_aspect_ratio() {
        let geometry = TerminalGeometry::with_cell_size(80, 24, 10, 20);
        assert_eq!(display_size(1920, 1080, 1, 1, 1.0, geometry), (76, 22));
    }

    #[test]
    fn video_fit_applies_sample_aspect_ratio() {
        let geometry = TerminalGeometry::with_cell_size(120, 40, 10, 20);
        assert_eq!(display_size(320, 240, 2, 1, 1.0, geometry), (64, 12));
    }
}
