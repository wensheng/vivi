use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::cli::Config;
use crate::ffmpeg::{AudioInfo, VideoInfo};
use crate::protocol::anchor::{self, AnchorKey};
use crate::protocol::media::{self, VideoPacket};
use crate::protocol::messages::{
    self, Credits, ImageSourceConfig, NodeConfig, SourceReady, VideoSourceConfig,
};
use crate::protocol::wire::{Connection, ConnectionKind, Endpoint, Record};

const SYNTHETIC_CREDITS: u64 = u64::MAX / 4;
const MAX_PENDING_CONTROL_RECORDS: usize = 4096;
const CONPTY_ANCHOR_TRANSPORT: &str = "conpty";

#[derive(Debug)]
pub struct SourceHandle {
    pub id: u64,
    ticket: Vec<u8>,
    credits: Credits,
    record_limit: u64,
    visible: bool,
    need_keyframe_epoch: Option<u32>,
}

pub struct MediaChannel {
    connection: Connection,
    source_id: u64,
}

pub struct VividClient {
    control: Connection,
    endpoint: Option<Endpoint>,
    trace_dir: Option<PathBuf>,
    dry_run: bool,
    verbose: bool,
    next_request_id: u64,
    next_object_id: u64,
    root_context_id: u64,
    display_generation: u64,
    session_tag: [u8; 16],
    anchor_key: AnchorKey,
    accepted_features: Vec<u64>,
    pending_control: VecDeque<Record>,
}

impl VividClient {
    pub fn connect(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let dry_run = config.is_dry_run();
        let endpoint = config
            .endpoint
            .as_deref()
            .map(Endpoint::parse)
            .transpose()?;
        let mut control = if let Some(trace_dir) = &config.trace_dir {
            Connection::trace(&trace_dir.join("control.vivid"), ConnectionKind::Control)?
        } else if dry_run {
            Connection::sink(ConnectionKind::Control)?
        } else {
            Connection::open(
                endpoint.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint")
                })?,
                ConnectionKind::Control,
            )?
        };

        let dry_token = "00".repeat(32);
        let token = config.token.as_deref().unwrap_or(&dry_token);
        let token_bytes = anchor::decode_token(token)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let hello_request = 1;
        control.write_record(
            messages::HELLO,
            0,
            0,
            &messages::hello(hello_request, token),
        )?;

        let (root_context_id, display_generation, session_tag, accepted_features, control_limit) =
            if dry_run {
                if config.verbose {
                    eprintln!("vivi: dry-run HELLO (Vivid 1.1)");
                }
                (
                    1,
                    0,
                    [0; 16],
                    vec![
                        messages::FEATURE_RASTER_RGBA8,
                        messages::FEATURE_SCENE_TRANSACTIONS,
                        messages::FEATURE_GRID_CELL_NODES,
                        messages::FEATURE_CREDIT_FLOW_CONTROL,
                        messages::FEATURE_ENCODED_IMAGE_V1,
                        messages::FEATURE_RASTER_ZSTD_V1,
                        messages::FEATURE_RASTER_PREMULTIPLIED_ALPHA,
                        messages::FEATURE_VISIBILITY_EVENTS_V1,
                        messages::FEATURE_VIDEO_ACCESS_UNIT_V1,
                        messages::FEATURE_VIDEO_CONTROL_V1,
                        messages::FEATURE_TEXT_ANCHORS_V2,
                        messages::FEATURE_AUDIO_ACCESS_UNIT_V1,
                    ],
                    crate::protocol::CONTROL_MAX_RECORD_BODY,
                )
            } else {
                let record =
                    read_expected(&mut control, hello_request, &[messages::WELCOME], None)?;
                let welcome = messages::parse_welcome(&record.body)?;
                if (welcome.selected_major, welcome.selected_minor) != (1, 1) {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "presenter selected Vivid {}.{}, expected 1.1",
                            welcome.selected_major, welcome.selected_minor
                        ),
                    )
                    .into());
                }
                for required in [
                    messages::FEATURE_RASTER_RGBA8,
                    messages::FEATURE_SCENE_TRANSACTIONS,
                    messages::FEATURE_GRID_CELL_NODES,
                    messages::FEATURE_CREDIT_FLOW_CONTROL,
                    messages::FEATURE_TEXT_ANCHORS_V2,
                ] {
                    if welcome.accepted_features.binary_search(&required).is_err() {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!("presenter did not accept required Vivid feature {required}"),
                        )
                        .into());
                    }
                }
                let session_tag: [u8; 16] =
                    welcome.session_tag.as_slice().try_into().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "WELCOME session tag is not 128 bits",
                        )
                    })?;
                if config.verbose {
                    eprintln!(
                        "vivi: session={} tag={} bytes root={} grid={}x{} generation={}",
                        welcome.session_id,
                        welcome.session_tag.len(),
                        welcome.root_context_id,
                        welcome.grid_columns,
                        welcome.grid_rows,
                        welcome.display_generation
                    );
                }
                (
                    welcome.root_context_id,
                    welcome.display_generation,
                    session_tag,
                    welcome.accepted_features,
                    welcome.maximum_control_body,
                )
            };
        control.set_send_body_limit(control_limit)?;
        let anchor_key = anchor::derive_key(&token_bytes, &session_tag);

        Ok(Self {
            control,
            endpoint,
            trace_dir: config.trace_dir.clone(),
            dry_run,
            verbose: config.verbose,
            next_request_id: hello_request,
            next_object_id: 0,
            root_context_id,
            display_generation,
            session_tag,
            anchor_key,
            accepted_features,
            pending_control: VecDeque::new(),
        })
    }

    pub fn allocate_id(&mut self) -> io::Result<u64> {
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Vivid object ID space exhausted"))?;
        Ok(self.next_object_id)
    }

    pub fn create_raster_source(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
    ) -> io::Result<SourceHandle> {
        let request_id = self.request_id()?;
        let body = messages::create_raster_config(
            request_id,
            &messages::RasterSourceConfig {
                source_id,
                width,
                height,
                alpha_mode: messages::ALPHA_STRAIGHT,
                compression_mode: if self.supports(messages::FEATURE_RASTER_ZSTD_V1) {
                    messages::COMPRESSION_RAW_OR_ZSTD
                } else {
                    messages::COMPRESSION_NONE
                },
            },
        );
        self.control
            .write_record(messages::CREATE_RASTER, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "raster")
    }

    pub fn create_video_source(
        &mut self,
        source_id: u64,
        info: &VideoInfo,
    ) -> io::Result<SourceHandle> {
        let probe_request = self.request_id()?;
        let probe = messages::probe_video_config(
            probe_request,
            &VideoSourceConfig {
                source_id: 0,
                codec: &info.codec,
                packetization: &info.packetization,
                extradata: &info.extradata,
                width: info.width,
                height: info.height,
                profile: info.profile,
                level: info.level,
                bitrate: info.bitrate,
                color_primaries: info.color_primaries,
                transfer: info.transfer,
                matrix: info.matrix,
                range: info.range,
                sar_num: info.sar_num,
                sar_den: info.sar_den,
                max_access_unit_bytes: info.max_access_unit_bytes,
            },
        );
        self.control
            .write_record(messages::PROBE_VIDEO_CONFIG, 0, 0, &probe)?;
        if !self.dry_run {
            let support = self.wait_for_reply(probe_request, &[messages::VIDEO_SUPPORT])?;
            if !messages::parse_video_support(&support.body)? {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "presenter cannot decode codec={} packetization={} at {}x{}",
                        info.codec, info.packetization, info.width, info.height
                    ),
                ));
            }
        }

        let request_id = self.request_id()?;
        let body = messages::create_video(
            request_id,
            &VideoSourceConfig {
                source_id,
                codec: &info.codec,
                packetization: &info.packetization,
                extradata: &info.extradata,
                width: info.width,
                height: info.height,
                profile: info.profile,
                level: info.level,
                bitrate: info.bitrate,
                color_primaries: info.color_primaries,
                transfer: info.transfer,
                matrix: info.matrix,
                range: info.range,
                sar_num: info.sar_num,
                sar_den: info.sar_den,
                max_access_unit_bytes: info.max_access_unit_bytes,
            },
        );
        self.control
            .write_record(messages::CREATE_VIDEO, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "video")
    }

    pub fn create_audio_source(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &AudioInfo,
    ) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_AUDIO_ACCESS_UNIT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks audio-access-unit-v1",
            ));
        }
        let config = messages::AudioSourceConfig {
            source_id,
            linked_video_source_id,
            codec: &info.codec,
            packetization: &info.packetization,
            extradata: &info.extradata,
            sample_rate: info.sample_rate,
            channels: info.channels,
            channel_mask: info.channel_mask,
            bitrate: info.bitrate,
            max_access_unit_bytes: info.max_access_unit_bytes,
        };
        let probe_request = self.request_id()?;
        self.control.write_record(
            messages::PROBE_AUDIO_CONFIG,
            0,
            0,
            &messages::probe_audio_config(
                probe_request,
                &messages::AudioSourceConfig {
                    source_id: 0,
                    linked_video_source_id: None,
                    codec: &info.codec,
                    packetization: &info.packetization,
                    extradata: &info.extradata,
                    sample_rate: info.sample_rate,
                    channels: info.channels,
                    channel_mask: info.channel_mask,
                    bitrate: info.bitrate,
                    max_access_unit_bytes: info.max_access_unit_bytes,
                },
            ),
        )?;
        if !self.dry_run {
            let support = self.wait_for_reply(probe_request, &[messages::AUDIO_SUPPORT])?;
            if !messages::parse_audio_support(&support.body)? {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("presenter cannot decode audio codec={}", info.codec),
                ));
            }
        }
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::CREATE_AUDIO,
            0,
            source_id,
            &messages::create_audio(request_id, &config),
        )?;
        self.source_ready(request_id, source_id, "audio")
    }

    pub fn create_image_source(&mut self, config: &ImageSourceConfig) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_ENCODED_IMAGE_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks encoded-image-v1",
            ));
        }
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::CREATE_IMAGE,
            0,
            config.source_id,
            &messages::create_image(request_id, config),
        )?;
        self.source_ready(request_id, config.source_id, "image")
    }

    pub fn place_source(
        &mut self,
        source_id: u64,
        node_id: u64,
        anchor_id: Option<u64>,
        columns: u32,
        rows: u32,
    ) -> io::Result<()> {
        let transaction_id = self.allocate_id()?;
        let begin_request = self.request_id()?;
        self.control.write_record(
            messages::BEGIN_TXN,
            0,
            0,
            &messages::begin_transaction(begin_request, transaction_id),
        )?;

        let (x, y) = if anchor_id.is_none() && !self.dry_run {
            crossterm::cursor::position()
                .map(|(column, row)| (i64::from(column) << 32, i64::from(row) << 32))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let node_request = self.request_id()?;
        self.control.write_record(
            messages::CREATE_NODE,
            0,
            node_id,
            &messages::create_node_at(
                node_request,
                transaction_id,
                NodeConfig {
                    node_id,
                    source_id,
                    context_id: self.root_context_id,
                    columns,
                    rows,
                    anchor_id,
                },
                x,
                y,
            ),
        )?;

        let mut stale_retries = 0;
        'commit: loop {
            let commit_request = self.request_id()?;
            self.control.write_record(
                messages::COMMIT_TXN,
                0,
                0,
                &messages::commit_transaction(
                    commit_request,
                    transaction_id,
                    self.display_generation,
                ),
            )?;
            if self.dry_run {
                break;
            }

            let record = self.wait_for_reply_raw(
                commit_request,
                &[messages::OK, messages::PRESENTED, messages::ERROR],
            )?;
            if record.record_type == messages::ERROR {
                let error = messages::parse_error_reply(&record.body)?;
                if error.code == messages::ERROR_STALE_DISPLAY_GENERATION && stale_retries < 3 {
                    stale_retries += 1;
                    continue 'commit;
                }
                return Err(io::Error::other(format!(
                    "presenter error {}: {}",
                    error.code, error.diagnostic
                )));
            }
            break 'commit;
        }
        if self.verbose {
            eprintln!(
                "vivi: placed source {source_id} as node {node_id} in {columns}x{rows} cells"
            );
        }
        Ok(())
    }

    /// Insert an authenticated marker at the current terminal cursor. APC transports wait for the
    /// presenter to attach it before continuing. ConPTY uses a scanner-compatible envelope and
    /// permits the node and marker to arrive in either order because it can defer terminal output
    /// while the producer is blocked.
    pub fn create_text_anchor(&mut self) -> io::Result<Option<u64>> {
        if std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some() {
            return Ok(None);
        }
        if self.dry_run {
            return self.allocate_id().map(Some);
        }
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let anchor_id = u64::from_be_bytes(bytes);
        if anchor_id == 0 {
            return self.create_text_anchor();
        }

        let marker = anchor::encode_marker(&self.anchor_key, &self.session_tag, anchor_id)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let conpty_transport = uses_conpty_anchor_transport();
        let marker = marker_for_transport(marker, conpty_transport);
        let mut stdout = io::stdout().lock();
        stdout.write_all(marker.as_bytes())?;
        stdout.flush()?;
        drop(stdout);

        if conpty_transport {
            if self.verbose {
                eprintln!(
                    "vivi: submitted asynchronous text anchor {anchor_id} over ConPTY transport"
                );
            }
            return Ok(Some(anchor_id));
        }

        loop {
            let record = if let Some(index) = self.pending_control.iter().position(|record| {
                record.record_type == messages::ANCHOR_READY && record.object_id == anchor_id
            }) {
                self.pending_control
                    .remove(index)
                    .expect("pending record exists")
            } else {
                self.control.read_record()?
            };
            match record.record_type {
                messages::ANCHOR_READY
                    if messages::parse_anchor_event(&record.body)? == anchor_id =>
                {
                    return Ok(Some(anchor_id));
                }
                messages::DISPLAY_CHANGED => {
                    self.display_generation =
                        messages::parse_display_changed(&record.body)?.display_generation;
                }
                messages::ERROR => {
                    return Err(io::Error::other(messages::parse_error(&record.body)?));
                }
                _ => self.queue_control(record)?,
            }
        }
    }

    pub fn open_media_channel(
        &self,
        source: &SourceHandle,
        kind: ConnectionKind,
    ) -> io::Result<MediaChannel> {
        let mut connection = if let Some(trace_dir) = &self.trace_dir {
            let label = match kind {
                ConnectionKind::Video => "video",
                ConnectionKind::Raster => "raster",
                ConnectionKind::Blob => "blob",
                ConnectionKind::Control => "control",
                ConnectionKind::LocalBuffer => "buffer",
                ConnectionKind::Audio => "audio",
            };
            Connection::trace(
                &trace_dir.join(format!("{label}-{}.vivid", source.id)),
                kind,
            )?
        } else if self.dry_run {
            Connection::sink(kind)?
        } else {
            Connection::open(
                self.endpoint.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint")
                })?,
                kind,
            )?
        };
        connection.write_record(
            messages::ATTACH_CHANNEL,
            0,
            source.id,
            &messages::attach_channel(&source.ticket),
        )?;
        connection.set_send_body_limit(u32::try_from(source.record_limit).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "source body limit exceeds u32")
        })?)?;
        Ok(MediaChannel {
            connection,
            source_id: source.id,
        })
    }

    pub fn send_raster_frame(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        epoch: u32,
        frame_id: u64,
        size: (u32, u32),
        rgba: &[u8],
    ) -> io::Result<()> {
        let (width, height) = size;
        let raw = media::raster_frame_body(epoch, frame_id, width, height, rgba)?;
        let body = if self.supports(messages::FEATURE_RASTER_ZSTD_V1) {
            let compressed = media::raster_frame_body_with_compression(
                epoch, frame_id, width, height, rgba, true,
            )?;
            if compressed.len() < raw.len() {
                compressed
            } else {
                raw
            }
        } else {
            raw
        };
        self.validate_record_size(source, body.len() as u64)?;
        self.consume_credits(source, body.len() as u64)?;
        channel
            .connection
            .write_record(messages::RASTER_FRAME, 0, channel.source_id, &body)?;
        self.wait_for_media_credit(source)
    }

    pub fn send_video_packet(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        packet: VideoPacket<'_>,
    ) -> io::Result<()> {
        let body = media::video_packet_body(packet)?;
        self.validate_record_size(source, body.len() as u64)?;
        self.consume_credits(source, body.len() as u64)?;
        channel
            .connection
            .write_record(messages::VIDEO_PACKET, 0, channel.source_id, &body)
    }

    pub fn send_audio_packet(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        packet: media::AudioPacket<'_>,
    ) -> io::Result<()> {
        let body = media::audio_packet_body(packet)?;
        self.validate_record_size(source, body.len() as u64)?;
        self.consume_credits(source, body.len() as u64)?;
        channel
            .connection
            .write_record(messages::AUDIO_PACKET, 0, channel.source_id, &body)
    }

    pub fn send_image_data(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        encoded: &[u8],
    ) -> io::Result<()> {
        self.validate_record_size(source, encoded.len() as u64)?;
        self.consume_credits(source, encoded.len() as u64)?;
        channel
            .connection
            .write_record(messages::IMAGE_DATA, 0, channel.source_id, encoded)?;
        self.wait_for_media_credit(source)
    }

    pub fn supports(&self, feature: u64) -> bool {
        self.accepted_features.binary_search(&feature).is_ok()
    }

    pub fn play(&mut self, source_id: u64, minimum_buffer_us: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::PLAY,
            0,
            source_id,
            &messages::play(request_id, source_id, minimum_buffer_us),
        )?;
        self.wait_for_ok(request_id)
    }

    pub fn eos(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::EOS,
            0,
            source_id,
            &messages::eos(request_id, source_id, epoch),
        )?;
        self.wait_for_ok(request_id)
    }

    pub fn drain(&mut self, source_id: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::DRAIN,
            0,
            source_id,
            &messages::drain(request_id, source_id),
        )?;
        self.wait_for_ok(request_id)
    }

    pub fn pause(&mut self, source_id: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::PAUSE,
            0,
            source_id,
            &messages::pause(request_id, source_id),
        )?;
        self.wait_for_ok(request_id)
    }

    pub fn flush(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::FLUSH,
            0,
            source_id,
            &messages::flush(request_id, source_id, epoch),
        )?;
        self.wait_for_ok(request_id)
    }

    pub fn wait_until_visible(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        self.apply_pending_source_events(source)?;
        while !source.visible {
            let record = if let Some(index) = self.pending_control.iter().position(|record| {
                is_source_event(record.record_type) && record.object_id == source.id
            }) {
                self.pending_control
                    .remove(index)
                    .expect("pending record exists")
            } else {
                self.control.read_record()?
            };
            if (is_source_event(record.record_type) && record.object_id == source.id)
                || record.record_type == messages::DISPLAY_CHANGED
                || record.record_type == messages::ERROR
            {
                self.apply_source_event(source, record)?;
            } else {
                self.queue_control(record)?;
            }
        }
        Ok(())
    }

    pub fn apply_pending_source_events(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        while let Some(index) = self
            .pending_control
            .iter()
            .position(|record| is_source_event(record.record_type) && record.object_id == source.id)
        {
            let record = self
                .pending_control
                .remove(index)
                .expect("pending record exists");
            self.apply_source_event(source, record)?;
        }
        Ok(())
    }

    pub fn goodbye(&mut self) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control
            .write_record(messages::GOODBYE, 0, 0, &messages::goodbye(request_id))?;
        if !self.dry_run {
            let _ = self.wait_for_reply(request_id, &[messages::OK])?;
        }
        Ok(())
    }

    pub fn verbose(&self, message: impl std::fmt::Display) {
        if self.verbose {
            eprintln!("vivi: {message}");
        }
    }

    fn source_ready(
        &mut self,
        request_id: u64,
        source_id: u64,
        kind: &str,
    ) -> io::Result<SourceHandle> {
        let ready = if self.dry_run {
            let mut ticket = vec![0; 32];
            ticket[24..].copy_from_slice(&source_id.to_be_bytes());
            SourceReady {
                source_id,
                media_ticket: ticket,
                byte_credits: SYNTHETIC_CREDITS,
                packet_credits: SYNTHETIC_CREDITS,
                fragment_credits: SYNTHETIC_CREDITS,
                max_media_body: crate::protocol::HARD_MAX_RECORD_BODY,
            }
        } else {
            let record = self.wait_for_reply(request_id, &[messages::SOURCE_READY])?;
            messages::parse_source_ready(&record.body)?
        };
        if ready.source_id != source_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "SOURCE_READY is for {}, expected {source_id}",
                    ready.source_id
                ),
            ));
        }
        if self.verbose {
            eprintln!(
                "vivi: {kind} source {source_id} ready with {} byte / {} packet credits",
                ready.byte_credits, ready.packet_credits
            );
        }
        Ok(SourceHandle {
            id: source_id,
            ticket: ready.media_ticket,
            credits: Credits {
                bytes: ready.byte_credits,
                packets: ready.packet_credits,
                fragments: ready.fragment_credits,
            },
            record_limit: u64::from(ready.max_media_body),
            visible: true,
            need_keyframe_epoch: None,
        })
    }

    fn validate_record_size(&self, source: &SourceHandle, bytes: u64) -> io::Result<()> {
        if bytes > source.record_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Vivid media record is {bytes} bytes, exceeding source {}'s {}-byte credit window; fragmentation is required",
                    source.id, source.record_limit
                ),
            ));
        }
        Ok(())
    }

    fn consume_credits(&mut self, source: &mut SourceHandle, bytes: u64) -> io::Result<()> {
        while source.credits.bytes < bytes || source.credits.packets == 0 {
            if self.dry_run {
                return Err(io::Error::other("synthetic dry-run credits exhausted"));
            }
            let record = if let Some(index) = self.pending_control.iter().position(|record| {
                is_source_event(record.record_type) && record.object_id == source.id
            }) {
                self.pending_control
                    .remove(index)
                    .expect("pending record exists")
            } else {
                self.control.read_record()?
            };
            match record.record_type {
                messages::CREDIT if record.object_id == source.id => {
                    let added = messages::parse_credit(&record.body)?;
                    source.credits.bytes = source.credits.bytes.saturating_add(added.bytes);
                    source.credits.packets = source.credits.packets.saturating_add(added.packets);
                    source.credits.fragments =
                        source.credits.fragments.saturating_add(added.fragments);
                }
                messages::ERROR => {
                    return Err(io::Error::other(messages::parse_error(&record.body)?));
                }
                messages::DISPLAY_CHANGED => {
                    self.display_generation =
                        messages::parse_display_changed(&record.body)?.display_generation;
                }
                messages::VISIBILITY if record.object_id == source.id => {
                    source.visible = messages::parse_visibility(&record.body)?.visible;
                }
                messages::NEED_KEYFRAME if record.object_id == source.id => {
                    source.need_keyframe_epoch =
                        Some(messages::parse_need_keyframe(&record.body)?.minimum_epoch);
                }
                messages::SOURCE_LOST if record.object_id == source.id => {
                    return Err(source_lost_error(&record)?);
                }
                _ => self.queue_control(record)?,
            }
        }
        source.credits.bytes -= bytes;
        source.credits.packets -= 1;
        Ok(())
    }

    /// Wait until the presenter has consumed a one-shot image or raster record before the media
    /// channel and control session are allowed to close.
    fn wait_for_media_credit(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        if self.dry_run {
            return Ok(());
        }
        loop {
            let record = if let Some(index) = self.pending_control.iter().position(|record| {
                is_source_event(record.record_type) && record.object_id == source.id
            }) {
                self.pending_control
                    .remove(index)
                    .expect("pending record exists")
            } else {
                self.control.read_record()?
            };
            let media_consumed =
                record.record_type == messages::CREDIT && record.object_id == source.id;
            if (is_source_event(record.record_type) && record.object_id == source.id)
                || record.record_type == messages::DISPLAY_CHANGED
                || record.record_type == messages::ERROR
            {
                self.apply_source_event(source, record)?;
                if media_consumed {
                    return Ok(());
                }
            } else {
                self.queue_control(record)?;
            }
        }
    }

    fn apply_source_event(&mut self, source: &mut SourceHandle, record: Record) -> io::Result<()> {
        match record.record_type {
            messages::CREDIT if record.object_id == source.id => {
                let added = messages::parse_credit(&record.body)?;
                source.credits.bytes = source.credits.bytes.saturating_add(added.bytes);
                source.credits.packets = source.credits.packets.saturating_add(added.packets);
                source.credits.fragments = source.credits.fragments.saturating_add(added.fragments);
            }
            messages::VISIBILITY if record.object_id == source.id => {
                source.visible = messages::parse_visibility(&record.body)?.visible;
            }
            messages::NEED_KEYFRAME if record.object_id == source.id => {
                source.need_keyframe_epoch =
                    Some(messages::parse_need_keyframe(&record.body)?.minimum_epoch);
            }
            messages::DISPLAY_CHANGED => {
                self.display_generation =
                    messages::parse_display_changed(&record.body)?.display_generation;
            }
            messages::ERROR => return Err(io::Error::other(messages::parse_error(&record.body)?)),
            messages::SOURCE_LOST if record.object_id == source.id => {
                return Err(source_lost_error(&record)?);
            }
            _ => {}
        }
        Ok(())
    }

    fn request_id(&mut self) -> io::Result<u64> {
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Vivid request ID space exhausted"))?;
        Ok(self.next_request_id)
    }

    fn wait_for_reply(&mut self, request_id: u64, accepted: &[u16]) -> io::Result<Record> {
        let record = self.wait_for_reply_raw(request_id, accepted)?;
        if record.record_type == messages::ERROR {
            return Err(io::Error::other(messages::parse_error(&record.body)?));
        }
        Ok(record)
    }

    fn wait_for_ok(&mut self, request_id: u64) -> io::Result<()> {
        if !self.dry_run {
            self.wait_for_reply(request_id, &[messages::OK])?;
        }
        Ok(())
    }

    fn wait_for_reply_raw(&mut self, request_id: u64, accepted: &[u16]) -> io::Result<Record> {
        loop {
            if let Some(index) = self.pending_control.iter().position(|record| {
                (accepted.contains(&record.record_type) || record.record_type == messages::ERROR)
                    && messages::request_id(&record.body).ok() == Some(request_id)
            }) {
                return Ok(self
                    .pending_control
                    .remove(index)
                    .expect("pending record exists"));
            }
            let record = self.control.read_record()?;
            if record.record_type == messages::DISPLAY_CHANGED {
                self.display_generation =
                    messages::parse_display_changed(&record.body)?.display_generation;
                continue;
            }
            if (accepted.contains(&record.record_type) || record.record_type == messages::ERROR)
                && messages::request_id(&record.body)? == request_id
            {
                return Ok(record);
            }
            self.queue_control(record)?;
        }
    }

    fn queue_control(&mut self, record: Record) -> io::Result<()> {
        if self.pending_control.len() >= MAX_PENDING_CONTROL_RECORDS {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Vivid control dispatcher queue exceeded its bound",
            ));
        }
        self.pending_control.push_back(record);
        Ok(())
    }
}

fn uses_conpty_anchor_transport() -> bool {
    cfg!(windows)
        || configured_anchor_transport_is_conpty(
            std::env::var("VIVID_ANCHOR_TRANSPORT").ok().as_deref(),
        )
}

fn configured_anchor_transport_is_conpty(transport: Option<&str>) -> bool {
    transport == Some(CONPTY_ANCHOR_TRANSPORT)
}

fn marker_for_transport(marker: String, conpty_transport: bool) -> String {
    if conpty_transport {
        format!("{};VIVID-END", &marker[2..marker.len() - 2])
    } else {
        marker
    }
}

impl SourceHandle {
    pub fn take_keyframe_request(&mut self) -> Option<u32> {
        self.need_keyframe_epoch.take()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }
}

fn read_expected(
    connection: &mut Connection,
    expected_request_id: u64,
    accepted_types: &[u16],
    mut display_generation: Option<&mut u64>,
) -> io::Result<Record> {
    loop {
        let record = connection.read_record()?;
        if record.record_type == messages::ERROR {
            return Err(io::Error::other(messages::parse_error(&record.body)?));
        }
        if record.record_type == messages::DISPLAY_CHANGED {
            if let Some(generation) = display_generation.as_deref_mut() {
                *generation = messages::parse_display_changed(&record.body)?.display_generation;
            }
            continue;
        }
        if !accepted_types.contains(&record.record_type) {
            continue;
        }
        if messages::request_id(&record.body)? == expected_request_id {
            return Ok(record);
        }
    }
}

fn is_source_event(record_type: u16) -> bool {
    matches!(
        record_type,
        messages::CREDIT | messages::VISIBILITY | messages::NEED_KEYFRAME | messages::SOURCE_LOST
    )
}

fn source_lost_error(record: &Record) -> io::Result<io::Error> {
    let lost = messages::parse_source_lost(&record.body)?;
    if lost.source_id != record.object_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SOURCE_LOST object ID mismatch",
        ));
    }
    Ok(io::Error::other(format!(
        "Vivid source {} was lost ({}): {}",
        lost.source_id, lost.code, lost.diagnostic
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_anchor_uses_v2_marker() {
        let key = anchor::derive_key(&[0; 32], &[0; 16]);
        let marker = anchor::encode_marker(&key, &[0; 16], 7).unwrap();
        assert!(marker.starts_with("\x1b_VIVID;2;A;"));
        assert!(marker.len() <= 128);
    }

    #[test]
    fn remote_conpty_transport_selects_windows_marker_envelope() {
        let key = anchor::derive_key(&[0; 32], &[0; 16]);
        let marker = anchor::encode_marker(&key, &[0; 16], 7).unwrap();

        assert!(configured_anchor_transport_is_conpty(Some("conpty")));
        assert!(!configured_anchor_transport_is_conpty(None));
        assert!(!configured_anchor_transport_is_conpty(Some("apc")));

        let transported = marker_for_transport(marker.clone(), true);
        assert!(transported.starts_with("VIVID;2;A;"));
        assert!(transported.ends_with(";VIVID-END"));
        assert!(!transported.contains('\x1b'));
        assert_eq!(marker_for_transport(marker.clone(), false), marker);
    }
}
