use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::audio_player;
use crate::cli::Config;
use crate::client::{MediaChannel, SourceHandle, VividClient};
use crate::ffmpeg::{EncodedMediaPacket, EncodedPacket, VideoDemuxer};
use crate::protocol::media::{AudioPacket, VideoPacket};
use crate::protocol::wire::ConnectionKind;
use crate::terminal_geometry::{TerminalGeometry, cells_for_pixels, reserve_rows};

const FIT_MARGIN_COLS: u16 = 4;
const FIT_MARGIN_ROWS: u16 = 2;
const INITIAL_BUFFER_US: u64 = 33_000;
const AUDIO_PREBUFFER_US: u64 = 100_000;

struct PlaybackState {
    packet_id: u64,
    encoded_bytes: u64,
    playback_started: bool,
    audio_started: bool,
    playback_wall_start: Option<Instant>,
    first_pts_us: Option<i64>,
    last_pts_us: Option<i64>,
    epoch: u32,
    awaiting_keyframe: bool,
    audio_buffered_us: u64,
    audio_horizon_us: Option<i64>,
}

impl PlaybackState {
    fn new() -> Self {
        Self {
            packet_id: 0,
            encoded_bytes: 0,
            playback_started: false,
            audio_started: false,
            playback_wall_start: None,
            first_pts_us: None,
            last_pts_us: None,
            epoch: 1,
            awaiting_keyframe: false,
            audio_buffered_us: 0,
            audio_horizon_us: None,
        }
    }

    fn observe_audio_packet(&mut self, pts_us: i64, duration_us: u64) {
        self.audio_buffered_us = self.audio_buffered_us.saturating_add(duration_us);
        let duration_us = i64::try_from(duration_us).unwrap_or(i64::MAX);
        if pts_us != i64::MIN {
            let end = pts_us.saturating_add(duration_us);
            self.audio_horizon_us = Some(self.audio_horizon_us.map_or(end, |last| last.max(end)));
        } else if let Some(horizon) = self.audio_horizon_us.as_mut() {
            *horizon = horizon.saturating_add(duration_us);
        }
    }
}

fn audio_covers_video(packet: &EncodedPacket, audio_horizon_us: Option<i64>) -> bool {
    let timestamp = if packet.pts_us != i64::MIN {
        packet.pts_us
    } else {
        packet.dts_us
    };
    timestamp == i64::MIN || audio_horizon_us.is_some_and(|horizon| timestamp <= horizon)
}

fn start_playback(
    client: &mut VividClient,
    source_id: u64,
    minimum_buffer_us: u64,
    audio: &mut Option<audio_player::AudioPlayback>,
    state: &mut PlaybackState,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    client.play(source_id, minimum_buffer_us)?;
    state.playback_started = true;
    state.playback_wall_start = Some(Instant::now());
    if !state.audio_started
        && let Some(playback) = audio.as_ref()
    {
        match playback.start() {
            Ok(()) => state.audio_started = true,
            Err(error) => {
                eprintln!(
                    "vivi: warning: could not start audio for {}: {error}; continuing without sound",
                    path.display()
                );
                *audio = None;
            }
        }
    }
    Ok(())
}

struct VideoSubmitter<'a> {
    client: &'a mut VividClient,
    path: &'a Path,
    source_id: u64,
    source: &'a mut SourceHandle,
    channel: &'a mut MediaChannel,
    audio: &'a mut Option<audio_player::AudioPlayback>,
}

impl VideoSubmitter<'_> {
    fn submit(
        &mut self,
        state: &mut PlaybackState,
        packet: EncodedPacket,
        start_after_packet: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.client.apply_pending_source_events(self.source)?;
        if let Some(minimum_epoch) = self.source.take_keyframe_request() {
            state.epoch = state.epoch.saturating_add(1).max(minimum_epoch);
            state.awaiting_keyframe = true;
        }
        if state.awaiting_keyframe && !packet.key {
            return Ok(());
        }
        if state.awaiting_keyframe {
            self.client.flush(self.source_id, state.epoch)?;
            state.awaiting_keyframe = false;
            state.playback_started = false;
            state.audio_buffered_us = 0;
            state.audio_horizon_us = None;
        }
        if !self.source.is_visible() {
            if state.audio_started
                && let Some(playback) = self.audio.as_ref()
            {
                playback.pause();
            }
            self.client.pause(self.source_id)?;
            self.client.wait_until_visible(self.source)?;
            self.client.play(self.source_id, INITIAL_BUFFER_US)?;
            if state.audio_started
                && let Some(playback) = self.audio.as_ref()
            {
                playback.resume();
            }
        }
        if packet.data.is_empty() {
            return Ok(());
        }
        state.packet_id = state
            .packet_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("video packet ID space exhausted"))?;
        state.encoded_bytes = state.encoded_bytes.saturating_add(packet.data.len() as u64);
        if packet.pts_us != i64::MIN {
            state.first_pts_us.get_or_insert(packet.pts_us);
            state.last_pts_us = Some(
                state
                    .last_pts_us
                    .map_or(packet.pts_us, |last: i64| last.max(packet.pts_us)),
            );
        }
        self.client.send_video_packet(
            self.source,
            self.channel,
            VideoPacket {
                epoch: state.epoch,
                packet_id: state.packet_id,
                pts_us: packet.pts_us,
                dts_us: packet.dts_us,
                duration_us: 0,
                key: packet.key,
                data: &packet.data,
            },
        )?;

        if !state.playback_started && start_after_packet {
            start_playback(
                self.client,
                self.source_id,
                INITIAL_BUFFER_US,
                self.audio,
                state,
                self.path,
            )?;
        }
        if state.packet_id.is_multiple_of(120) {
            self.client.verbose(format_args!(
                "video source {}: sent {} packets / {} bytes",
                self.source_id, state.packet_id, state.encoded_bytes
            ));
        }
        Ok(())
    }
}

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
    let vivid_audio_available = info.audio.is_some()
        && client.supports(crate::protocol::messages::FEATURE_AUDIO_ACCESS_UNIT_V1)
        && !config.no_wait;
    let remote_session = std::env::var_os("VIVID_REMOTE").is_some();
    let mut audio: Option<audio_player::AudioPlayback> = if info.has_audio
        && !vivid_audio_available
        && !config.no_wait
        && !config.is_dry_run()
        && !remote_session
    {
        match audio_player::prepare_video(path, info.first_pts_us) {
            Ok(playback) => Some(playback),
            Err(error) => {
                eprintln!(
                    "vivi: warning: could not prepare audio for {}: {error}; continuing without sound",
                    path.display()
                );
                None
            }
        }
    } else {
        None
    };
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
    let mut vivid_audio = if vivid_audio_available {
        let audio_id = client.allocate_id()?;
        match client.create_audio_source(audio_id, Some(source_id), info.audio.as_ref().unwrap()) {
            Ok(audio_source) => {
                let audio_channel =
                    client.open_media_channel(&audio_source, ConnectionKind::Audio)?;
                Some((audio_id, audio_source, audio_channel, 0_u64))
            }
            Err(error) => {
                if !remote_session && !config.is_dry_run() {
                    match audio_player::prepare_video(path, info.first_pts_us) {
                        Ok(playback) => audio = Some(playback),
                        Err(fallback_error) => eprintln!(
                            "vivi: warning: could not create presenter audio for {}: {error}; direct audio fallback also failed: {fallback_error}; continuing without sound",
                            path.display(),
                        ),
                    }
                } else {
                    eprintln!(
                        "vivi: warning: could not create presenter audio for {}: {error}; continuing without sound",
                        path.display()
                    );
                }
                None
            }
        }
    } else {
        if info.has_audio && remote_session && !config.no_wait {
            eprintln!(
                "vivi: warning: presenter lacks remote audio for {}; continuing without sound",
                path.display()
            );
        }
        None
    };
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

    let mut state = PlaybackState::new();
    // The media channels are independent, so preserve each stream's decode order rather than the
    // container's cross-stream packet order. Some MP4s front-load enough video to fill the video
    // socket before their first audio packet; buffering those video access units lets audio reach
    // the presenter and start its master clock without a circular wait.
    let mut pending_video = VecDeque::new();
    while let Some(media_packet) = demuxer.next_media_packet()? {
        match media_packet {
            EncodedMediaPacket::Audio(packet) => {
                let mut failed = None;
                if let Some((_, audio_source, audio_channel, packet_id)) = vivid_audio.as_mut()
                    && !packet.data.is_empty()
                {
                    *packet_id = packet_id
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("audio packet ID space exhausted"))?;
                    if let Err(error) = client.send_audio_packet(
                        audio_source,
                        audio_channel,
                        AudioPacket {
                            epoch: state.epoch,
                            packet_id: *packet_id,
                            pts_us: packet.pts_us,
                            dts_us: packet.dts_us,
                            duration_us: packet.duration_us,
                            trim_start_samples: packet.trim_start_samples,
                            trim_end_samples: packet.trim_end_samples,
                            data: &packet.data,
                        },
                    ) {
                        failed = Some(error);
                    } else {
                        state.observe_audio_packet(packet.pts_us, packet.duration_us);
                    }
                }
                if let Some(error) = failed {
                    eprintln!(
                        "vivi: warning: presenter audio failed for {}: {error}; continuing without sound",
                        path.display()
                    );
                    vivid_audio = None;
                }
            }
            EncodedMediaPacket::Video(packet) => {
                if vivid_audio.is_some() {
                    pending_video.push_back(packet);
                } else {
                    VideoSubmitter {
                        client,
                        path,
                        source_id,
                        source: &mut source,
                        channel: &mut channel,
                        audio: &mut audio,
                    }
                    .submit(&mut state, packet, true)?;
                }
            }
        }

        if vivid_audio.is_some() {
            if !state.playback_started {
                while pending_video.front().is_some_and(|packet| !packet.key) {
                    pending_video.pop_front();
                }
                if pending_video.front().is_some_and(|packet| packet.key)
                    && state.audio_buffered_us >= AUDIO_PREBUFFER_US
                {
                    start_playback(
                        client,
                        source_id,
                        AUDIO_PREBUFFER_US,
                        &mut audio,
                        &mut state,
                        path,
                    )?;
                }
            }
            while state.playback_started
                && pending_video
                    .front()
                    .is_some_and(|packet| audio_covers_video(packet, state.audio_horizon_us))
            {
                let packet = pending_video
                    .pop_front()
                    .expect("pending video packet exists");
                VideoSubmitter {
                    client,
                    path,
                    source_id,
                    source: &mut source,
                    channel: &mut channel,
                    audio: &mut audio,
                }
                .submit(&mut state, packet, false)?;
            }
        } else {
            while let Some(packet) = pending_video.pop_front() {
                VideoSubmitter {
                    client,
                    path,
                    source_id,
                    source: &mut source,
                    channel: &mut channel,
                    audio: &mut audio,
                }
                .submit(&mut state, packet, true)?;
            }
        }
    }

    if state.packet_id == 0 && !pending_video.is_empty() && !state.playback_started {
        start_playback(
            client,
            source_id,
            state.audio_buffered_us,
            &mut audio,
            &mut state,
            path,
        )?;
    }
    while let Some(packet) = pending_video.pop_front() {
        VideoSubmitter {
            client,
            path,
            source_id,
            source: &mut source,
            channel: &mut channel,
            audio: &mut audio,
        }
        .submit(&mut state, packet, true)?;
    }

    if state.packet_id == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "video has no packets").into());
    }
    if !state.playback_started {
        start_playback(
            client,
            source_id,
            state.audio_buffered_us,
            &mut audio,
            &mut state,
            path,
        )?;
    }
    client.eos(source_id, state.epoch)?;
    if let Some((audio_id, _, _, _)) = vivid_audio.as_ref()
        && let Err(error) = client
            .eos(*audio_id, state.epoch)
            .and_then(|_| client.drain(*audio_id))
    {
        eprintln!(
            "vivi: warning: presenter audio drain failed for {}: {error}; video playback completed",
            path.display()
        );
    }
    if !config.no_wait
        && let (Some(started), Some(first_pts), Some(last_pts)) = (
            state.playback_wall_start,
            state.first_pts_us,
            state.last_pts_us,
        )
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
    if state.audio_started
        && let Some(playback) = audio.as_mut()
        && let Err(error) = playback.wait()
    {
        eprintln!(
            "vivi: warning: audio playback failed for {}: {error}; video playback completed",
            path.display()
        );
    }
    client.verbose(format_args!(
        "video source {source_id}: EOS after {} packets / {} bytes",
        state.packet_id, state.encoded_bytes
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

    #[test]
    fn audio_horizon_gates_video_from_front_loaded_muxes() {
        let mut state = PlaybackState::new();
        for pts_us in [-21_333, 0, 21_333, 42_667, 64_000] {
            state.observe_audio_packet(pts_us, 21_333);
        }
        assert!(state.audio_buffered_us >= AUDIO_PREBUFFER_US);
        assert_eq!(state.audio_horizon_us, Some(85_333));

        let covered = EncodedPacket {
            data: vec![1],
            pts_us: 83_333,
            dts_us: 0,
            key: true,
        };
        let video_ahead_of_audio = EncodedPacket {
            data: vec![2],
            pts_us: 250_000,
            dts_us: 41_667,
            key: false,
        };
        assert!(audio_covers_video(&covered, state.audio_horizon_us));
        assert!(!audio_covers_video(
            &video_ahead_of_audio,
            state.audio_horizon_us
        ));
        assert!(!audio_covers_video(&covered, None));
    }
}
