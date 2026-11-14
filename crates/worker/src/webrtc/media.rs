use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::Feed;
use crate::settings::VideoQuality;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Codec {
    Av1,
    H265,
    H264,
    Vp9,
    Vp8,
}

impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Codec::Av1 => "av1",
            Codec::H265 => "h265",
            Codec::H264 => "h264",
            Codec::Vp9 => "vp9",
            Codec::Vp8 => "vp8",
        }
    }
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

fn ffmpeg(bin: &Path, args: Vec<String>) -> Result<Child> {
    Command::new(bin)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot start {}", bin.display()))
}

fn log_stderr(child: &mut Child, what: &'static str) {
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("Svt[info]") || line.starts_with("Svt[warn]") {
                    tracing::debug!(target: "ffmpeg", encoder = what, "{line}");
                } else {
                    tracing::warn!(target: "ffmpeg", encoder = what, "{line}");
                }
            }
        });
    }
}

fn input_args() -> Vec<String> {
    ["-hide_banner", "-loglevel", "error", "-nofind_stream_info", "-probesize", "32", "-analyzeduration", "0"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn crf(codec: Codec, quality: VideoQuality) -> &'static str {
    match (codec, quality) {
        (Codec::Av1, VideoQuality::Low) => "40",
        (Codec::Av1, VideoQuality::Medium) => "32",
        (Codec::Av1, VideoQuality::High) => "26",
        (Codec::Av1, VideoQuality::Max) => "20",
        (Codec::H264, VideoQuality::Low) => "30",
        (Codec::H264, VideoQuality::Medium) => "23",
        (Codec::H264, VideoQuality::High) => "19",
        (Codec::H264, VideoQuality::Max) => "15",
        (Codec::H265, VideoQuality::Low) => "33",
        (Codec::H265, VideoQuality::Medium) => "28",
        (Codec::H265, VideoQuality::High) => "24",
        (Codec::H265, VideoQuality::Max) => "20",
        (Codec::Vp9, VideoQuality::Low) => "44",
        (Codec::Vp9, VideoQuality::Medium) => "36",
        (Codec::Vp9, VideoQuality::High) => "30",
        (Codec::Vp9, VideoQuality::Max) => "22",
        (Codec::Vp8, VideoQuality::Low) => "40",
        (Codec::Vp8, VideoQuality::Medium) => "28",
        (Codec::Vp8, VideoQuality::High) => "18",
        (Codec::Vp8, VideoQuality::Max) => "10",
    }
}

pub fn video_args(codec: Codec, width: usize, height: usize, fps: u32, quality: VideoQuality) -> Vec<String> {
    let mut a = input_args();
    a.extend(
        [
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgr0",
            "-s",
            &format!("{width}x{height}"),
            "-framerate",
            &fps.to_string(),
            "-use_wallclock_as_timestamps",
            "1",
            "-i",
            "pipe:0",
            "-fps_mode",
            "passthrough",
            "-vf",
            "crop=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p",
            "-g",
            "600",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    let crf = crf(codec, quality);
    let enc: &[&str] = match codec {
        Codec::Av1 => {
            &["-c:v", "libsvtav1", "-preset", "12", "-crf", crf, "-svtav1-params", "pred-struct=1:scm=1", "-f", "ivf"]
        }
        Codec::H264 => &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-crf",
            crf,
            "-maxrate",
            "20M",
            "-bufsize",
            "6M",
            "-f",
            "flv",
        ],
        Codec::H265 => &[
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "main",
            "-crf",
            crf,
            "-maxrate",
            "20M",
            "-bufsize",
            "6M",
            "-x265-params",
            "log-level=error",
            "-f",
            "flv",
        ],
        Codec::Vp9 => &[
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-lag-in-frames",
            "0",
            "-row-mt",
            "1",
            "-crf",
            crf,
            "-b:v",
            "20M",
            "-f",
            "ivf",
        ],
        Codec::Vp8 => &[
            "-c:v",
            "libvpx",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-lag-in-frames",
            "0",
            "-crf",
            crf,
            "-b:v",
            "20M",
            "-f",
            "ivf",
        ],
    };
    a.extend(enc.iter().map(|s| s.to_string()));
    a.extend(["-flush_packets", "1", "pipe:1"].iter().map(|s| s.to_string()));
    a
}

pub fn audio_args() -> Vec<String> {
    let mut a = input_args();
    a.extend(
        [
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
            "pipe:0",
            "-c:a",
            "libopus",
            "-b:a",
            "128k",
            "-application",
            "lowdelay",
            "-frame_duration",
            "20",
            "-f",
            "ogg",
            "-page_duration",
            "20000",
            "-flush_packets",
            "1",
            "pipe:1",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a
}

pub struct VideoEncoder {
    pub width: usize,
    pub height: usize,
    frames: watch::Sender<Option<Arc<Frame>>>,
    tasks: [JoinHandle<()>; 2],
    _child: Child,
}

impl VideoEncoder {
    pub fn spawn(
        bin: &Path,
        codec: Codec,
        (width, height): (usize, usize),
        (fps, quality): (u32, VideoQuality),
        out: mpsc::Sender<Feed>,
    ) -> Result<Self> {
        let mut child = ffmpeg(bin, video_args(codec, width, height, fps, quality))?;
        log_stderr(&mut child, codec.label());
        let mut stdin = child.stdin.take().context("encoder stdin")?;
        let mut stdout = child.stdout.take().context("encoder stdout")?;
        let (frames, mut rx) = watch::channel::<Option<Arc<Frame>>>(None);
        let writer = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let frame = rx.borrow_and_update().clone();
                if let Some(f) = frame
                    && f.width == width
                    && f.height == height
                    && stdin.write_all(&f.data).await.is_err()
                {
                    break;
                }
            }
        });
        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; 256 * 1024];
            let mut ivf = Ivf::default();
            let mut flv = Flv::default();
            loop {
                let n = match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let frames = match codec {
                    Codec::H264 | Codec::H265 => flv.push(&buf[..n]),
                    _ => ivf.push(&buf[..n]).into_iter().map(|f| (keyframe(codec, &f), f)).collect(),
                };
                for (keyframe, data) in frames {
                    if out.send(Feed::Video { codec, data, keyframe }).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(VideoEncoder { width, height, frames, tasks: [writer, reader], _child: child })
    }

    pub fn push(&self, frame: Arc<Frame>) {
        self.frames.send_replace(Some(frame));
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub async fn stream_audio(bin: &Path, socket: &Path, out: &mpsc::Sender<Feed>) -> Result<()> {
    let conn = tokio::net::UnixStream::connect(socket).await.context("sound is unavailable")?;
    let mut child = ffmpeg(bin, audio_args())?;
    log_stderr(&mut child, "opus");
    let mut stdin = child.stdin.take().context("encoder stdin")?;
    let mut stdout = child.stdout.take().context("encoder stdout")?;
    let (mut pcm, _keep) = conn.into_split();
    let feed = async {
        let _ = tokio::io::copy(&mut pcm, &mut stdin).await;
    };
    let read = async {
        let mut buf = vec![0u8; 16 * 1024];
        let mut ogg = Ogg::default();
        loop {
            let n = match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for packet in ogg.push(&buf[..n]) {
                if out.send(Feed::Audio(packet)).await.is_err() {
                    return;
                }
            }
        }
    };
    tokio::select! {
        _ = feed => {}
        _ = read => {}
    }
    Ok(())
}

#[derive(Default)]
pub struct Ivf {
    buf: Vec<u8>,
    started: bool,
}

impl Ivf {
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        if !self.started {
            if self.buf.len() < 32 {
                return out;
            }
            let header = u16::from_le_bytes([self.buf[6], self.buf[7]]) as usize;
            if self.buf.len() < header {
                return out;
            }
            self.buf.drain(..header);
            self.started = true;
        }
        let mut at = 0;
        while self.buf.len() >= at + 12 {
            let size = u32::from_le_bytes(self.buf[at..at + 4].try_into().unwrap()) as usize;
            if self.buf.len() < at + 12 + size {
                break;
            }
            out.push(self.buf[at + 12..at + 12 + size].to_vec());
            at += 12 + size;
        }
        self.buf.drain(..at);
        out
    }
}

pub fn keyframe(codec: Codec, frame: &[u8]) -> bool {
    match codec {
        Codec::Av1 => av1_keyframe(frame),
        Codec::Vp9 => frame.first().is_some_and(|b| b >> 6 == 2 && b & 0x0c == 0),
        Codec::Vp8 => frame.first().is_some_and(|b| b & 1 == 0),
        Codec::H264 | Codec::H265 => false,
    }
}

pub fn av1_keyframe(tu: &[u8]) -> bool {
    let mut at = 0;
    while at < tu.len() {
        let header = tu[at];
        let kind = (header >> 3) & 0x0f;
        if kind == 1 {
            return true;
        }
        let mut i = at + 1 + ((header >> 2) & 1) as usize;
        if header & 2 == 0 {
            return false;
        }
        let mut size = 0usize;
        for shift in 0..8 {
            let Some(&b) = tu.get(i) else { return false };
            i += 1;
            size |= ((b & 0x7f) as usize) << (shift * 7);
            if b & 0x80 == 0 {
                break;
            }
        }
        at = i + size;
    }
    false
}

#[derive(Default)]
pub struct Flv {
    buf: Vec<u8>,
    started: bool,
    params: Vec<u8>,
    length_size: usize,
}

const START: [u8; 4] = [0, 0, 0, 1];

impl Flv {
    pub fn push(&mut self, data: &[u8]) -> Vec<(bool, Vec<u8>)> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        if !self.started {
            if self.buf.len() < 13 {
                return out;
            }
            let header = u32::from_be_bytes(self.buf[5..9].try_into().unwrap()) as usize;
            if self.buf.len() < header + 4 {
                return out;
            }
            self.buf.drain(..header + 4);
            self.started = true;
        }
        let mut at = 0;
        while self.buf.len() >= at + 11 {
            let size = u32::from_be_bytes([0, self.buf[at + 1], self.buf[at + 2], self.buf[at + 3]]) as usize;
            if self.buf.len() < at + 11 + size + 4 {
                break;
            }
            if self.buf[at] == 9 {
                let tag = self.buf[at + 11..at + 11 + size].to_vec();
                if let Some(frame) = self.video_tag(&tag) {
                    out.push(frame);
                }
            }
            at += 11 + size + 4;
        }
        self.buf.drain(..at);
        out
    }

    fn video_tag(&mut self, tag: &[u8]) -> Option<(bool, Vec<u8>)> {
        let first = *tag.first()?;
        let (kind, hevc, body) = if first & 0x80 != 0 {
            if tag.get(1..5)? != b"hvc1" {
                return None;
            }
            match first & 0x0f {
                0 => (0, true, &tag[5..]),
                1 => (1, true, tag.get(8..)?),
                3 => (1, true, &tag[5..]),
                _ => return None,
            }
        } else if first & 0x0f == 7 {
            (*tag.get(1)?, false, tag.get(5..)?)
        } else {
            return None;
        };
        match kind {
            0 => {
                (self.length_size, self.params) =
                    if hevc { hevc_config(body) } else { avc_config(body) }.unwrap_or_default();
                None
            }
            1 => {
                if self.length_size == 0 {
                    return None;
                }
                let keyframe = (first >> 4) & 7 == 1;
                let mut annexb = if keyframe { self.params.clone() } else { Vec::new() };
                let mut i = 0;
                while i + self.length_size <= body.len() {
                    let len = body[i..i + self.length_size].iter().fold(0usize, |acc, &b| (acc << 8) | b as usize);
                    i += self.length_size;
                    annexb.extend_from_slice(&START);
                    annexb.extend_from_slice(body.get(i..i + len)?);
                    i += len;
                }
                Some((keyframe, annexb))
            }
            _ => None,
        }
    }
}

fn param_sets(body: &[u8], mut i: usize, count: usize, out: &mut Vec<u8>) -> Option<usize> {
    for _ in 0..count {
        let len = u16::from_be_bytes([*body.get(i)?, *body.get(i + 1)?]) as usize;
        out.extend_from_slice(&START);
        out.extend_from_slice(body.get(i + 2..i + 2 + len)?);
        i += 2 + len;
    }
    Some(i)
}

fn avc_config(body: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut params = Vec::new();
    let i = param_sets(body, 6, (*body.get(5)? & 0x1f) as usize, &mut params)?;
    param_sets(body, i + 1, *body.get(i)? as usize, &mut params)?;
    Some(((*body.get(4)? & 3) as usize + 1, params))
}

fn hevc_config(body: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut params = Vec::new();
    let mut i = 23;
    for _ in 0..*body.get(22)? {
        let count = u16::from_be_bytes([*body.get(i + 1)?, *body.get(i + 2)?]) as usize;
        i = param_sets(body, i + 3, count, &mut params)?;
    }
    Some(((*body.get(21)? & 3) as usize + 1, params))
}

#[derive(Default)]
pub struct Ogg {
    buf: Vec<u8>,
    packet: Vec<u8>,
}

impl Ogg {
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut at = 0;
        while self.buf.len() >= at + 27 {
            if &self.buf[at..at + 4] != b"OggS" {
                at += 1;
                continue;
            }
            let segments = self.buf[at + 26] as usize;
            if self.buf.len() < at + 27 + segments {
                break;
            }
            let lacing = &self.buf[at + 27..at + 27 + segments];
            let body: usize = lacing.iter().map(|&l| l as usize).sum();
            let start = at + 27 + segments;
            if self.buf.len() < start + body {
                break;
            }
            let mut p = start;
            for &l in lacing {
                self.packet.extend_from_slice(&self.buf[p..p + l as usize]);
                p += l as usize;
                if l < 255 {
                    let packet = std::mem::take(&mut self.packet);
                    if !packet.starts_with(b"OpusHead") && !packet.starts_with(b"OpusTags") {
                        out.push(packet);
                    }
                }
            }
            at = start + body;
        }
        self.buf.drain(..at);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_in_pieces<T>(bytes: &[u8], mut push: impl FnMut(&[u8]) -> Vec<T>) -> Vec<T> {
        let mut out = Vec::new();
        for chunk in bytes.chunks(3) {
            out.extend(push(chunk));
        }
        out
    }

    #[test]
    fn ivf_frames_are_split_even_when_bytes_trickle_in() {
        let mut s = b"DKIF\0\0\x20\0AV01".to_vec();
        s.resize(32, 0);
        for f in [&b"\x0a\x01\x02"[..], &b"\x32\x00"[..]] {
            s.extend_from_slice(&(f.len() as u32).to_le_bytes());
            s.extend_from_slice(&[0; 8]);
            s.extend_from_slice(f);
        }
        let mut ivf = Ivf::default();
        let frames = feed_in_pieces(&s, |c| ivf.push(c));
        assert_eq!(frames, vec![b"\x0a\x01\x02".to_vec(), b"\x32\x00".to_vec()]);
    }

    #[test]
    fn av1_keyframes_carry_a_sequence_header() {
        assert!(av1_keyframe(&[0x12, 0x00, 0x0a, 0x01, 0xff, 0x32, 0x00]));
        assert!(!av1_keyframe(&[0x12, 0x00, 0x32, 0x02, 0xaa, 0xbb]));
        assert!(!av1_keyframe(&[0x12, 0x00, 0x32]));
    }

    #[test]
    fn flv_h264_becomes_annexb_with_parameter_sets_on_keyframes() {
        let mut s = b"FLV\x01\x01\0\0\0\x09\0\0\0\0".to_vec();
        let tag = |body: Vec<u8>| {
            let mut t = vec![9u8];
            t.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            t.extend_from_slice(&[0; 7]);
            t.extend_from_slice(&body);
            t.extend_from_slice(&((body.len() + 11) as u32).to_be_bytes());
            t
        };
        s.extend(tag(vec![0x17, 0, 0, 0, 0, 1, 0x42, 0xe0, 0x1f, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 1, 0x68]));
        s.extend(tag(vec![0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0xaa]));
        s.extend(tag(vec![0x27, 1, 0, 0, 0, 0, 0, 0, 1, 0x41, 0, 0, 0, 1, 0x06]));
        let mut flv = Flv::default();
        let frames = feed_in_pieces(&s, |c| flv.push(c));
        assert_eq!(frames.len(), 2);
        assert!(frames[0].0);
        assert_eq!(frames[0].1, vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0, 0, 0, 1, 0x65, 0xaa]);
        assert!(!frames[1].0);
        assert_eq!(frames[1].1, vec![0, 0, 0, 1, 0x41, 0, 0, 0, 1, 0x06]);
    }

    #[test]
    fn ogg_pages_yield_opus_packets_without_headers() {
        let page = |segments: &[u8], body: &[u8]| {
            let mut p = b"OggS".to_vec();
            p.extend_from_slice(&[0; 22]);
            p.push(segments.len() as u8);
            p.extend_from_slice(segments);
            p.extend_from_slice(body);
            p
        };
        let mut s = page(&[19], b"OpusHead___________");
        s.extend(page(&[3, 2], b"abcde"));
        let big = vec![7u8; 300];
        s.extend(page(&[255], &big[..255]));
        s.extend(page(&[45], &big[255..]));
        let mut ogg = Ogg::default();
        let packets = feed_in_pieces(&s, |c| ogg.push(c));
        assert_eq!(packets, vec![b"abc".to_vec(), b"de".to_vec(), big]);
    }

    #[tokio::test]
    async fn every_codec_turns_screen_frames_into_decodable_video() {
        let bin = Path::new("/usr/bin/ffmpeg");
        if !bin.exists() {
            return;
        }
        for codec in [Codec::Av1, Codec::H265, Codec::H264, Codec::Vp9, Codec::Vp8] {
            let (tx, mut rx) = mpsc::channel(256);
            let enc = VideoEncoder::spawn(bin, codec, (160, 128), (30, VideoQuality::Medium), tx).unwrap();
            let mut frames = Vec::new();
            for i in 0..100u32 {
                let mut data = vec![0u8; 160 * 128 * 4];
                data.iter_mut().enumerate().for_each(|(j, b)| *b = (j as u32 / 4 + i * 7) as u8);
                enc.push(Arc::new(Frame { width: 160, height: 128, data }));
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                while let Ok(Feed::Video { codec: c, data, keyframe }) = rx.try_recv() {
                    assert_eq!(c, codec);
                    frames.push((keyframe, data));
                }
                if frames.len() >= 10 {
                    break;
                }
            }
            assert!(frames.len() >= 10, "{codec:?} gave {} frames", frames.len());
            assert!(frames[0].0, "{codec:?} must start with a keyframe");
            assert!(frames[1..].iter().all(|f| !f.0), "{codec:?} marked a delta frame as a keyframe");
            let format = match codec {
                Codec::H264 => "h264",
                Codec::H265 => "hevc",
                _ => continue,
            };
            let out = std::process::Command::new(bin)
                .args(["-v", "error", "-f", format, "-i", "pipe:0", "-f", "framecrc", "pipe:1"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut p| {
                    let mut stdin = p.stdin.take().unwrap();
                    for (_, f) in &frames {
                        std::io::Write::write_all(&mut stdin, f)?;
                    }
                    drop(stdin);
                    p.wait_with_output()
                })
                .unwrap();
            let decoded = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#')).count();
            assert!(
                out.status.success() && out.stderr.is_empty(),
                "{codec:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(decoded, frames.len(), "{codec:?} frames did not all decode");
        }
    }

    #[test]
    fn vp8_and_vp9_keyframes_are_recognized() {
        assert!(keyframe(Codec::Vp8, &[0x50, 0x2a]) && !keyframe(Codec::Vp8, &[0x51, 0x2a]));
        assert!(keyframe(Codec::Vp9, &[0x82, 0x49]) && !keyframe(Codec::Vp9, &[0x86, 0x00]));
        assert!(!keyframe(Codec::Vp9, &[0x8a]) && !keyframe(Codec::Vp8, &[]));
    }

    #[test]
    fn encoders_are_tuned_for_low_delay() {
        let av1 = video_args(Codec::Av1, 1000, 1462, 30, VideoQuality::Medium).join(" ");
        assert!(av1.contains("-s 1000x1462") && av1.contains("libsvtav1") && av1.contains("pred-struct=1"));
        assert!(av1.contains("-nofind_stream_info") && av1.contains("-flush_packets 1") && av1.ends_with("pipe:1"));
        assert!(av1.contains("-framerate 30") && av1.contains("-crf 32"));
        let h264 = video_args(Codec::H264, 800, 600, 60, VideoQuality::Max).join(" ");
        assert!(h264.contains("zerolatency") && h264.contains("-profile:v baseline") && h264.contains("-f flv"));
        assert!(h264.contains("-framerate 60") && h264.contains("-crf 15"));
        assert!(video_args(Codec::Av1, 8, 8, 15, VideoQuality::Low).join(" ").contains("-crf 40"));
        let h265 = video_args(Codec::H265, 800, 600, 30, VideoQuality::High).join(" ");
        assert!(
            h265.contains("libx265")
                && h265.contains("zerolatency")
                && h265.contains("-crf 24")
                && h265.contains("-f flv")
        );
        for (codec, enc) in [(Codec::Vp9, "libvpx-vp9"), (Codec::Vp8, "libvpx ")] {
            let a = video_args(codec, 800, 600, 30, VideoQuality::Medium).join(" ");
            assert!(
                a.contains(enc)
                    && a.contains("-deadline realtime")
                    && a.contains("-lag-in-frames 0")
                    && a.contains("-f ivf")
            );
        }
        assert!(audio_args().join(" ").contains("libopus"));
    }
}
