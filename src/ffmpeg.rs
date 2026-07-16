//! Minimal libavformat bindings for Vivid's encoded-packet fast path.
//!
//! Unlike Kitim, Vivi does not decode video into RGBA frames. It demultiplexes the selected video
//! track and forwards encoded access units, timestamps, codec configuration, and keyframe flags.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::io;
use std::path::Path;
use std::ptr;

const AVMEDIA_TYPE_VIDEO: c_int = 0;
const AV_LOG_QUIET: c_int = -8;
const AV_NOPTS_VALUE: i64 = i64::MIN;
const AVCOL_RANGE_UNSPECIFIED: c_int = 0;
const AVCOL_PRI_UNSPECIFIED: c_int = 2;
const AVCOL_TRC_UNSPECIFIED: c_int = 2;
const AVCOL_SPC_UNSPECIFIED: c_int = 2;
const MAX_EXTRADATA: usize = 16 * 1024 * 1024;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AVChannelLayout {
    order: c_int,
    nb_channels: c_int,
    mask: u64,
    opaque: *mut c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AVRational {
    num: c_int,
    den: c_int,
}

#[cfg(not(ffmpeg_old_channel_layout))]
#[repr(C)]
struct AVCodecParameters {
    codec_type: c_int,
    codec_id: u32,
    codec_tag: u32,
    _pad1: u32,
    extradata: *mut u8,
    extradata_size: c_int,
    _pad2: u32,
    coded_side_data: *mut c_void,
    nb_coded_side_data: c_int,
    format: c_int,
    bit_rate: i64,
    bits_per_coded_sample: c_int,
    bits_per_raw_sample: c_int,
    profile: c_int,
    level: c_int,
    width: c_int,
    height: c_int,
    sample_aspect_ratio: AVRational,
    #[cfg(ffmpeg_codecpar_has_framerate)]
    framerate: AVRational,
    field_order: c_int,
    color_range: c_int,
    color_primaries: c_int,
    color_trc: c_int,
    color_space: c_int,
    chroma_location: c_int,
    video_delay: c_int,
    ch_layout: AVChannelLayout,
    sample_rate: c_int,
}

#[cfg(ffmpeg_old_channel_layout)]
#[repr(C)]
struct AVCodecParameters {
    codec_type: c_int,
    codec_id: u32,
    codec_tag: u32,
    extradata: *mut u8,
    extradata_size: c_int,
    format: c_int,
    bit_rate: i64,
    bits_per_coded_sample: c_int,
    bits_per_raw_sample: c_int,
    profile: c_int,
    level: c_int,
    width: c_int,
    height: c_int,
    sample_aspect_ratio: AVRational,
    field_order: c_int,
    color_range: c_int,
    color_primaries: c_int,
    color_trc: c_int,
    color_space: c_int,
    chroma_location: c_int,
    video_delay: c_int,
    channel_layout: u64,
    channels: c_int,
    sample_rate: c_int,
    block_align: c_int,
    frame_size: c_int,
    initial_padding: c_int,
    trailing_padding: c_int,
    seek_preroll: c_int,
    ch_layout: AVChannelLayout,
    framerate: AVRational,
    coded_side_data: *mut c_void,
    nb_coded_side_data: c_int,
}

#[repr(C)]
struct AVStream {
    av_class: *const c_void,
    index: c_int,
    id: c_int,
    codecpar: *mut AVCodecParameters,
    priv_data: *mut c_void,
    time_base: AVRational,
    start_time: i64,
    duration: i64,
    nb_frames: i64,
    disposition: c_int,
    discard: c_int,
    sample_aspect_ratio: AVRational,
    metadata: *mut c_void,
    avg_frame_rate: AVRational,
}

#[repr(C)]
struct AVFormatContext {
    av_class: *const c_void,
    iformat: *mut c_void,
    oformat: *mut c_void,
    priv_data: *mut c_void,
    pb: *mut c_void,
    ctx_flags: c_int,
    nb_streams: c_uint,
    streams: *mut *mut AVStream,
}

#[repr(C)]
struct AVPacket {
    buf: *mut c_void,
    pts: i64,
    dts: i64,
    data: *mut u8,
    size: c_int,
    stream_index: c_int,
    flags: c_int,
}

unsafe extern "C" {
    fn av_log_set_level(level: c_int);
    fn avformat_open_input(
        context: *mut *mut AVFormatContext,
        url: *const c_char,
        format: *mut c_void,
        options: *mut *mut c_void,
    ) -> c_int;
    fn avformat_find_stream_info(context: *mut AVFormatContext, options: *mut *mut c_void)
    -> c_int;
    fn avformat_close_input(context: *mut *mut AVFormatContext);
    fn av_read_frame(context: *mut AVFormatContext, packet: *mut AVPacket) -> c_int;
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_unref(packet: *mut AVPacket);
    fn av_packet_free(packet: *mut *mut AVPacket);
    fn avcodec_get_name(codec_id: u32) -> *const c_char;
    fn av_strerror(error: c_int, buffer: *mut c_char, buffer_size: usize) -> c_int;
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub codec: String,
    pub packetization: String,
    pub extradata: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub profile: i32,
    pub level: i32,
    pub bitrate: i64,
    pub color_primaries: u64,
    pub transfer: u64,
    pub matrix: u64,
    pub range: u64,
    pub colorimetry_inferred: bool,
    pub sar_num: u32,
    pub sar_den: u32,
    pub max_access_unit_bytes: u32,
}

#[derive(Debug)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub pts_us: i64,
    pub dts_us: i64,
    pub key: bool,
}

pub struct VideoDemuxer {
    context: *mut AVFormatContext,
    packet: *mut AVPacket,
    stream_index: c_int,
    time_base: AVRational,
    info: VideoInfo,
    nal_length_size: Option<usize>,
}

impl VideoDemuxer {
    pub fn open(path: &Path) -> io::Result<Self> {
        let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "media path contains a NUL byte",
            )
        })?;

        unsafe { av_log_set_level(AV_LOG_QUIET) };
        let mut context = ptr::null_mut();
        let result = unsafe {
            avformat_open_input(
                &mut context,
                path.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if result < 0 {
            return Err(ffmpeg_error("could not open media", result));
        }

        let stream_result = unsafe { avformat_find_stream_info(context, ptr::null_mut()) };
        if stream_result < 0 {
            unsafe { avformat_close_input(&mut context) };
            return Err(ffmpeg_error(
                "could not inspect media streams",
                stream_result,
            ));
        }

        let selected = unsafe { find_video_stream(context) };
        let (stream_index, stream, parameters) = match selected {
            Some(selected) => selected,
            None => {
                unsafe { avformat_close_input(&mut context) };
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "media has no video stream",
                ));
            }
        };

        let packet = unsafe { av_packet_alloc() };
        if packet.is_null() {
            unsafe { avformat_close_input(&mut context) };
            return Err(io::Error::other("FFmpeg could not allocate a packet"));
        }

        let (info, nal_length_size) = unsafe { video_info(parameters)? };
        let time_base = unsafe { (*stream).time_base };
        if time_base.num <= 0 || time_base.den <= 0 {
            let mut packet = packet;
            unsafe {
                av_packet_free(&mut packet);
                avformat_close_input(&mut context);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid video time base",
            ));
        }

        Ok(Self {
            context,
            packet,
            stream_index,
            time_base,
            info,
            nal_length_size,
        })
    }

    pub fn inspect(path: &Path) -> io::Result<VideoInfo> {
        let mut demuxer = Self::open(path)?;
        let mut maximum = 0_usize;
        while let Some(packet) = demuxer.next_packet()? {
            maximum = maximum.max(packet.data.len());
        }
        if maximum == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "video has no access units",
            ));
        }
        demuxer.info.max_access_unit_bytes = u32::try_from(maximum)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "access unit exceeds u32"))?;
        Ok(demuxer.info.clone())
    }

    pub fn next_packet(&mut self) -> io::Result<Option<EncodedPacket>> {
        loop {
            unsafe { av_packet_unref(self.packet) };
            let result = unsafe { av_read_frame(self.context, self.packet) };
            if result < 0 {
                return Ok(None);
            }

            let packet = unsafe { &*self.packet };
            if packet.stream_index != self.stream_index {
                continue;
            }
            if packet.size < 0 || (packet.size > 0 && packet.data.is_null()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid FFmpeg packet",
                ));
            }

            let mut data = if packet.size == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }.to_vec()
            };
            if let Some(length_size) = self.nal_length_size {
                data = length_prefixed_to_annex_b(&data, length_size)?;
            }
            return Ok(Some(EncodedPacket {
                key: vivid_protocol::media::access_unit_is_key(&self.info.codec, &data)?,
                data,
                pts_us: timestamp_us(packet.pts, self.time_base),
                dts_us: timestamp_us(packet.dts, self.time_base),
            }));
        }
    }
}

impl Drop for VideoDemuxer {
    fn drop(&mut self) {
        unsafe {
            av_packet_free(&mut self.packet);
            avformat_close_input(&mut self.context);
        }
    }
}

unsafe fn find_video_stream(
    context: *mut AVFormatContext,
) -> Option<(c_int, *mut AVStream, *mut AVCodecParameters)> {
    let context = unsafe { &*context };
    for index in 0..context.nb_streams as usize {
        let stream = unsafe { *context.streams.add(index) };
        if stream.is_null() {
            continue;
        }
        let parameters = unsafe { (*stream).codecpar };
        if !parameters.is_null() && unsafe { (*parameters).codec_type } == AVMEDIA_TYPE_VIDEO {
            return Some((index as c_int, stream, parameters));
        }
    }
    None
}

unsafe fn video_info(parameters: *mut AVCodecParameters) -> io::Result<(VideoInfo, Option<usize>)> {
    let parameters = unsafe { &*parameters };
    let width = u32::try_from(parameters.width)
        .ok()
        .filter(|width| *width > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid video width"))?;
    let height = u32::try_from(parameters.height)
        .ok()
        .filter(|height| *height > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid video height"))?;

    let codec_pointer = unsafe { avcodec_get_name(parameters.codec_id) };
    let codec = if codec_pointer.is_null() {
        format!("ffmpeg-codec-{}", parameters.codec_id)
    } else {
        unsafe { CStr::from_ptr(codec_pointer) }
            .to_string_lossy()
            .into_owned()
    };

    let extradata_size = usize::try_from(parameters.extradata_size)
        .ok()
        .filter(|size| *size <= MAX_EXTRADATA)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid codec extradata size")
        })?;
    let extradata = if extradata_size == 0 {
        Vec::new()
    } else if parameters.extradata.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "codec extradata pointer is null",
        ));
    } else {
        unsafe { std::slice::from_raw_parts(parameters.extradata, extradata_size) }.to_vec()
    };

    let (packetization, extradata, nal_length_size) = match codec.as_str() {
        "h264" => {
            let (data, length) = normalize_h26x_extradata(&extradata, false)?;
            ("h264-annexb-au-v1".to_owned(), data, length)
        }
        "hevc" => {
            let (data, length) = normalize_h26x_extradata(&extradata, true)?;
            ("hevc-annexb-au-v1".to_owned(), data, length)
        }
        "vp9" => ("vp9-frame-v1".to_owned(), Vec::new(), None),
        "av1" => ("av1-low-overhead-tu-v1".to_owned(), extradata, None),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("codec {codec:?} has no Vivid 1.1 portable packetization"),
            ));
        }
    };
    let colorimetry_inferred = parameters.color_primaries == AVCOL_PRI_UNSPECIFIED
        || parameters.color_trc == AVCOL_TRC_UNSPECIFIED
        || parameters.color_space == AVCOL_SPC_UNSPECIFIED
        || parameters.color_range == AVCOL_RANGE_UNSPECIFIED;
    let color_primaries = map_primaries(parameters.color_primaries, height)?;
    let transfer = map_transfer(parameters.color_trc)?;
    let matrix = map_matrix(parameters.color_space, height)?;
    let range = map_range(parameters.color_range)?;
    let sar = parameters.sample_aspect_ratio;
    let (sar_num, sar_den) = if sar.num > 0 && sar.den > 0 {
        (sar.num as u32, sar.den as u32)
    } else {
        (1, 1)
    };

    Ok((
        VideoInfo {
            packetization,
            codec,
            extradata,
            width,
            height,
            profile: parameters.profile,
            level: parameters.level,
            bitrate: parameters.bit_rate,
            color_primaries,
            transfer,
            matrix,
            range,
            colorimetry_inferred,
            sar_num,
            sar_den,
            max_access_unit_bytes: 0,
        },
        nal_length_size,
    ))
}

fn normalize_h26x_extradata(data: &[u8], hevc: bool) -> io::Result<(Vec<u8>, Option<usize>)> {
    if data.is_empty() || data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1]) {
        return Ok((data.to_vec(), None));
    }
    if data[0] != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported H.26x extradata",
        ));
    }
    let mut output = Vec::new();
    if hevc {
        if data.len() < 23 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated hvcC"));
        }
        let length_size = usize::from((data[21] & 3) + 1);
        let arrays = usize::from(data[22]);
        let mut offset = 23;
        for _ in 0..arrays {
            if offset + 3 > data.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated hvcC array",
                ));
            }
            offset += 1;
            let count = usize::from(u16::from_be_bytes(
                data[offset..offset + 2].try_into().unwrap(),
            ));
            offset += 2;
            for _ in 0..count {
                append_config_nal(data, &mut offset, &mut output)?;
            }
        }
        if offset != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing hvcC data",
            ));
        }
        Ok((output, Some(length_size)))
    } else {
        if data.len() < 7 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated avcC"));
        }
        let length_size = usize::from((data[4] & 3) + 1);
        let mut offset = 6;
        let sps = usize::from(data[5] & 0x1f);
        for _ in 0..sps {
            append_config_nal(data, &mut offset, &mut output)?;
        }
        if offset >= data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing avcC PPS count",
            ));
        }
        let pps = usize::from(data[offset]);
        offset += 1;
        for _ in 0..pps {
            append_config_nal(data, &mut offset, &mut output)?;
        }
        Ok((output, Some(length_size)))
    }
}

fn append_config_nal(data: &[u8], offset: &mut usize, output: &mut Vec<u8>) -> io::Result<()> {
    if *offset + 2 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated NAL length",
        ));
    }
    let length = usize::from(u16::from_be_bytes(
        data[*offset..*offset + 2].try_into().unwrap(),
    ));
    *offset += 2;
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "NAL exceeds extradata"))?;
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(&data[*offset..end]);
    *offset = end;
    Ok(())
}

fn length_prefixed_to_annex_b(data: &[u8], length_size: usize) -> io::Result<Vec<u8>> {
    if !(1..=4).contains(&length_size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NAL length size",
        ));
    }
    let mut output = Vec::with_capacity(data.len() + 16);
    let mut offset = 0;
    while offset < data.len() {
        if offset + length_size > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated access-unit NAL length",
            ));
        }
        let mut length = 0_usize;
        for byte in &data[offset..offset + length_size] {
            length = (length << 8) | usize::from(*byte);
        }
        offset += length_size;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "access-unit NAL exceeds packet")
            })?;
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&data[offset..end]);
        offset = end;
    }
    Ok(output)
}

fn map_primaries(value: c_int, height: u32) -> io::Result<u64> {
    match value {
        1 => Ok(1),
        5 => Ok(2),
        6 => Ok(3),
        9 => Ok(4),
        AVCOL_PRI_UNSPECIFIED => Ok(if height > 576 {
            1 // BT.709 for HD video.
        } else if height > 480 {
            2 // BT.601 625-line family.
        } else {
            3 // BT.601 525-line family.
        }),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "video color primaries are missing or unsupported",
        )),
    }
}
fn map_transfer(value: c_int) -> io::Result<u64> {
    match value {
        1 => Ok(1),
        13 => Ok(2),
        AVCOL_TRC_UNSPECIFIED => Ok(1),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "video transfer characteristic is missing or unsupported",
        )),
    }
}
fn map_matrix(value: c_int, height: u32) -> io::Result<u64> {
    match value {
        0 => Ok(0),
        1 => Ok(1),
        5 | 6 => Ok(2),
        9 => Ok(3),
        AVCOL_SPC_UNSPECIFIED => Ok(if height > 576 { 1 } else { 2 }),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "video matrix coefficients are missing or unsupported",
        )),
    }
}
fn map_range(value: c_int) -> io::Result<u64> {
    match value {
        1 => Ok(1),
        2 => Ok(2),
        AVCOL_RANGE_UNSPECIFIED => Ok(1),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "video signal range is missing or unsupported",
        )),
    }
}

fn timestamp_us(timestamp: i64, time_base: AVRational) -> i64 {
    if timestamp == AV_NOPTS_VALUE {
        return AV_NOPTS_VALUE;
    }
    let value = i128::from(timestamp)
        .saturating_mul(i128::from(time_base.num))
        .saturating_mul(1_000_000)
        / i128::from(time_base.den);
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn ffmpeg_error(context: &str, code: c_int) -> io::Error {
    let mut buffer = [0_i8; 256];
    let description = if unsafe { av_strerror(code, buffer.as_mut_ptr(), buffer.len()) } == 0 {
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    } else {
        format!("FFmpeg error {code}")
    };
    io::Error::other(format!("{context}: {description}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(
        target_pointer_width = "64",
        not(ffmpeg_old_channel_layout),
        ffmpeg_codecpar_has_framerate
    ))]
    #[test]
    fn ffmpeg_8_codec_parameters_color_layout_matches_abi() {
        assert_eq!(std::mem::offset_of!(AVCodecParameters, framerate), 88);
        assert_eq!(
            std::mem::offset_of!(AVCodecParameters, color_primaries),
            104
        );
    }

    #[test]
    fn timestamps_convert_to_microseconds() {
        assert_eq!(
            timestamp_us(
                90_000,
                AVRational {
                    num: 1,
                    den: 90_000
                }
            ),
            1_000_000
        );
        assert_eq!(
            timestamp_us(AV_NOPTS_VALUE, AVRational { num: 1, den: 1 }),
            i64::MIN
        );
    }

    #[test]
    fn unspecified_colorimetry_uses_sd_and_hd_defaults() {
        assert_eq!(map_primaries(AVCOL_PRI_UNSPECIFIED, 480).unwrap(), 3);
        assert_eq!(map_primaries(AVCOL_PRI_UNSPECIFIED, 576).unwrap(), 2);
        assert_eq!(map_primaries(AVCOL_PRI_UNSPECIFIED, 720).unwrap(), 1);
        assert_eq!(map_transfer(AVCOL_TRC_UNSPECIFIED).unwrap(), 1);
        assert_eq!(map_matrix(AVCOL_SPC_UNSPECIFIED, 480).unwrap(), 2);
        assert_eq!(map_matrix(AVCOL_SPC_UNSPECIFIED, 720).unwrap(), 1);
        assert_eq!(map_range(AVCOL_RANGE_UNSPECIFIED).unwrap(), 1);
    }

    #[test]
    fn declared_unsupported_colorimetry_still_fails() {
        assert!(map_primaries(22, 1080).is_err());
        assert!(map_transfer(22).is_err());
        assert!(map_matrix(22, 1080).is_err());
        assert!(map_range(22).is_err());
    }
}
