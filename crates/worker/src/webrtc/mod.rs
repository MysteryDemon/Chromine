pub mod capture;
pub mod media;
pub mod rfb;

use std::collections::{BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use base64::Engine;
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec as RtcCodec;
use str0m::media::{MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::config::WebrtcCodec;
use crate::settings::Live;
use capture::Command;
use media::Codec;

const MAX_VIEWERS: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const OPUS_FRAME: u64 = 960;
const MAX_QUEUED_BYTES: usize = 16 << 20;

pub struct Settings {
    pub vnc_socket: PathBuf,
    pub audio_socket: PathBuf,
    pub ffmpeg: PathBuf,
    pub port: u16,
    pub live: watch::Receiver<Live>,
}

pub enum Feed {
    Video { codec: Codec, data: Vec<u8>, keyframe: bool },
    Audio(Vec<u8>),
    Cursor(rfb::Cursor),
    Clipboard(String),
}

pub struct Joined {
    pub id: u64,
    pub answer: SdpAnswer,
    pub closed: oneshot::Receiver<()>,
}

enum Request {
    Join { offer: SdpOffer, candidate: SocketAddr, reply: oneshot::Sender<Result<Joined>> },
    Leave(u64),
}

#[derive(Clone)]
pub struct Hub {
    tx: mpsc::Sender<Request>,
    port: u16,
}

impl Hub {
    pub async fn start(settings: Settings) -> Result<Hub> {
        str0m::crypto::from_feature_flags().install_process_default();
        let socket = UdpSocket::bind(("0.0.0.0", settings.port))
            .await
            .with_context(|| format!("cannot open UDP port {} for WebRTC", settings.port))?;
        let port = socket.local_addr()?.port();
        let (tx, rx) = mpsc::channel(64);
        let (feed_tx, feed_rx) = mpsc::channel(256);
        let mut live = settings.live.clone();
        let codec = live.borrow_and_update().webrtc_codec;
        let hub = State {
            settings: Arc::new(settings),
            live,
            codec,
            socket,
            outbox: Outbox::default(),
            sessions: Vec::new(),
            next_id: 1,
            capture: None,
            audio: None,
            codecs: BTreeSet::new(),
            feed_tx,
            epoch: Instant::now(),
            audio_clock: None,
            cursor: None,
        };
        tokio::spawn(hub.run(rx, feed_rx));
        tracing::info!(port, "WebRTC streaming ready");
        Ok(Hub { tx, port })
    }

    pub async fn join(&self, offer: SdpOffer, candidate: SocketAddr) -> Result<Joined> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Request::Join { offer, candidate, reply }).await.map_err(|_| anyhow!("streaming stopped"))?;
        rx.await.map_err(|_| anyhow!("streaming stopped"))?
    }

    pub async fn leave(&self, id: u64) {
        let _ = self.tx.send(Request::Leave(id)).await;
    }
}

struct Session {
    id: u64,
    rtc: Rtc,
    video: Option<(Mid, Pt, Codec)>,
    audio: Option<(Mid, Pt)>,
    channel: Option<ChannelId>,
    connected: bool,
    waiting_keyframe: bool,
    created: Instant,
    local: SocketAddr,
    _closed: oneshot::Sender<()>,
}

struct State {
    settings: Arc<Settings>,
    live: watch::Receiver<Live>,
    codec: WebrtcCodec,
    socket: UdpSocket,
    outbox: Outbox,
    sessions: Vec<Session>,
    next_id: u64,
    capture: Option<(mpsc::Sender<Command>, JoinHandle<()>)>,
    audio: Option<JoinHandle<()>>,
    codecs: BTreeSet<Codec>,
    feed_tx: mpsc::Sender<Feed>,
    epoch: Instant,
    audio_clock: Option<(u64, Instant)>,
    cursor: Option<String>,
}

#[derive(Default)]
struct Outbox {
    queue: VecDeque<(Vec<u8>, SocketAddr)>,
    bytes: usize,
}

impl Outbox {
    fn send(&mut self, socket: &UdpSocket, data: &[u8], to: SocketAddr) {
        if self.queue.is_empty() {
            match socket.try_send_to(data, to) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                _ => return,
            }
        }
        if self.bytes + data.len() > MAX_QUEUED_BYTES {
            tracing::warn!("WebRTC send queue is full; dropping a packet");
            return;
        }
        self.bytes += data.len();
        self.queue.push_back((data.to_vec(), to));
    }

    fn flush(&mut self, socket: &UdpSocket) {
        while let Some((data, to)) = self.queue.front() {
            match socket.try_send_to(data, *to) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                _ => {
                    self.bytes -= data.len();
                    self.queue.pop_front();
                }
            }
        }
    }
}

const AUTO: [Codec; 4] = [Codec::Av1, Codec::H264, Codec::Vp9, Codec::Vp8];

fn rtc_for(choice: WebrtcCodec) -> Rtc {
    Rtc::builder()
        .set_ice_lite(true)
        .clear_codecs()
        .enable_opus(true, false)
        .enable_av1(true)
        .enable_h265(choice == WebrtcCodec::H265)
        .enable_h264(true)
        .enable_vp9(true)
        .enable_vp8(true)
        .build(Instant::now())
}

fn wanted(choice: WebrtcCodec) -> Option<Codec> {
    match choice {
        WebrtcCodec::Auto => None,
        WebrtcCodec::Av1 => Some(Codec::Av1),
        WebrtcCodec::H265 => Some(Codec::H265),
        WebrtcCodec::H264 => Some(Codec::H264),
        WebrtcCodec::Vp9 => Some(Codec::Vp9),
        WebrtcCodec::Vp8 => Some(Codec::Vp8),
    }
}

fn pick_video(choice: WebrtcCodec, params: &[(Pt, RtcCodec, bool)]) -> Option<(Pt, Codec)> {
    let find = |codec: Codec| {
        let ours = |p: &&(Pt, RtcCodec, bool)| match codec {
            Codec::Av1 => p.1 == RtcCodec::Av1,
            Codec::H265 => p.1 == RtcCodec::H265,
            Codec::H264 => p.1 == RtcCodec::H264,
            Codec::Vp9 => p.1 == RtcCodec::Vp9,
            Codec::Vp8 => p.1 == RtcCodec::Vp8,
        };
        params.iter().filter(ours).max_by_key(|p| p.2).map(|p| (p.0, codec))
    };
    wanted(choice).into_iter().chain(AUTO).find_map(find)
}

impl State {
    async fn run(mut self, mut requests: mpsc::Receiver<Request>, mut feed: mpsc::Receiver<Feed>) {
        let mut buf = vec![0u8; 2048];
        loop {
            let deadline = self.drive();
            self.tidy();
            tokio::select! {
                r = requests.recv() => match r {
                    Some(Request::Join { offer, candidate, reply }) => { let _ = reply.send(self.join(offer, candidate)); }
                    Some(Request::Leave(id)) => self.sessions.retain(|s| s.id != id),
                    None => return,
                },
                Some(f) = feed.recv() => self.deliver(f),
                r = self.socket.recv_from(&mut buf) => {
                    let Ok((n, source)) = r else { continue };
                    let Ok(contents) = buf[..n].try_into() else { continue };
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: source, contents });
                    if let Some(s) = self.sessions.iter_mut().find(|s| s.rtc.accepts(&input)) {
                        let Input::Receive(t, mut r) = input else { unreachable!() };
                        r.destination = s.local;
                        if let Err(e) = s.rtc.handle_input(Input::Receive(t, r)) {
                            tracing::debug!(error = %e, "WebRTC viewer dropped");
                            s.rtc.disconnect();
                        }
                    }
                }
                _ = self.socket.writable(), if !self.outbox.queue.is_empty() => self.outbox.flush(&self.socket),
                Ok(()) = self.live.changed() => {
                    let codec = self.live.borrow_and_update().webrtc_codec;
                    if codec != self.codec {
                        self.codec = codec;
                        tracing::info!(?codec, "WebRTC codec changed; viewers reconnect");
                        for s in &mut self.sessions {
                            s.rtc.disconnect();
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => {
                    let now = Instant::now();
                    for s in &mut self.sessions {
                        if s.rtc.handle_input(Input::Timeout(now)).is_err() {
                            s.rtc.disconnect();
                        }
                    }
                }
            }
        }
    }

    fn join(&mut self, offer: SdpOffer, candidate: SocketAddr) -> Result<Joined> {
        if self.sessions.len() >= MAX_VIEWERS {
            bail!("too many viewers are connected");
        }
        let mut rtc = rtc_for(self.codec);
        rtc.add_local_candidate(Candidate::host(candidate, "udp").context("bad WebRTC address")?);
        let answer = rtc.sdp_api().accept_offer(offer).context("the browser offer was not usable")?;
        let id = self.next_id;
        self.next_id += 1;
        let (closed_tx, closed) = oneshot::channel();
        self.sessions.push(Session {
            id,
            rtc,
            video: None,
            audio: None,
            channel: None,
            connected: false,
            waiting_keyframe: true,
            created: Instant::now(),
            local: candidate,
            _closed: closed_tx,
        });
        Ok(Joined { id, answer, closed })
    }

    fn drive(&mut self) -> Instant {
        let mut soonest = Instant::now() + Duration::from_secs(1);
        let mut commands = Vec::new();
        for s in &mut self.sessions {
            loop {
                if !s.rtc.is_alive() {
                    break;
                }
                match s.rtc.poll_output() {
                    Ok(Output::Timeout(t)) => {
                        soonest = soonest.min(t);
                        break;
                    }
                    Ok(Output::Transmit(t)) => self.outbox.send(&self.socket, &t.contents, t.destination),
                    Ok(Output::Event(e)) => s.event(e, self.codec, &mut commands, self.cursor.as_deref()),
                    Err(e) => {
                        tracing::debug!(error = %e, "WebRTC viewer failed");
                        s.rtc.disconnect();
                    }
                }
            }
        }
        for c in commands {
            self.command(c);
        }
        soonest
    }

    fn tidy(&mut self) {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.rtc.is_alive() && (s.connected || s.created.elapsed() < CONNECT_TIMEOUT));
        if self.sessions.len() != before {
            tracing::info!(viewers = self.sessions.len(), "WebRTC viewer left");
        }
        let codecs: BTreeSet<Codec> =
            self.sessions.iter().filter(|s| s.connected).filter_map(|s| s.video.map(|v| v.2)).collect();
        let watching = self.sessions.iter().any(|s| s.connected);
        if !watching {
            if let Some((_, task)) = self.capture.take() {
                task.abort();
            }
            if let Some(task) = self.audio.take() {
                task.abort();
            }
            self.codecs.clear();
            self.audio_clock = None;
            return;
        }
        if self.capture.is_none() {
            let (tx, rx) = mpsc::channel(256);
            let task = tokio::spawn(capture::run(self.settings.clone(), rx, self.feed_tx.clone()));
            self.capture = Some((tx, task));
            self.codecs.clear();
        }
        if codecs != self.codecs {
            self.codecs = codecs.clone();
            self.command(Command::Codecs(codecs));
        }
        if self.audio.is_none() && self.sessions.iter().any(|s| s.connected && s.audio.is_some()) {
            let settings = self.settings.clone();
            let out = self.feed_tx.clone();
            self.audio = Some(tokio::spawn(async move {
                loop {
                    if let Err(e) = media::stream_audio(&settings.ffmpeg, &settings.audio_socket, &out).await {
                        tracing::debug!(error = %e, "WebRTC sound unavailable");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }));
        }
    }

    fn command(&self, c: Command) {
        if let Some((tx, _)) = &self.capture {
            let _ = tx.try_send(c);
        }
    }

    fn deliver(&mut self, feed: Feed) {
        let now = Instant::now();
        match feed {
            Feed::Video { codec, data, keyframe } => {
                let at = MediaTime::from_micros(now.duration_since(self.epoch).as_micros() as u64);
                for s in self.sessions.iter_mut().filter(|s| s.connected) {
                    let Some((mid, pt, c)) = s.video else { continue };
                    if c != codec || (s.waiting_keyframe && !keyframe) {
                        continue;
                    }
                    s.waiting_keyframe = false;
                    if let Some(w) = s.rtc.writer(mid)
                        && let Err(e) = w.write(pt, now, at, data.clone())
                    {
                        tracing::debug!(error = %e, "video frame not sent");
                    }
                }
            }
            Feed::Audio(packet) => {
                let wall = now.duration_since(self.epoch).as_micros() as u64 * 48 / 1000;
                let samples = match self.audio_clock {
                    Some((last, at)) if now.duration_since(at) < Duration::from_millis(100) => last + OPUS_FRAME,
                    _ => wall,
                };
                self.audio_clock = Some((samples, now));
                let at = MediaTime::new(samples, str0m::media::Frequency::FORTY_EIGHT_KHZ);
                for s in self.sessions.iter_mut().filter(|s| s.connected) {
                    let Some((mid, pt)) = s.audio else { continue };
                    if let Some(w) = s.rtc.writer(mid) {
                        let _ = w.write(pt, now, at, packet.clone());
                    }
                }
            }
            Feed::Cursor(c) => {
                let msg = json!([
                    "cursor",
                    c.hot_x,
                    c.hot_y,
                    c.width,
                    c.height,
                    base64::engine::general_purpose::STANDARD.encode(&c.rgba)
                ])
                .to_string();
                self.broadcast(&msg);
                self.cursor = Some(msg);
            }
            Feed::Clipboard(text) => self.broadcast(&json!(["clip", text]).to_string()),
        }
    }

    fn broadcast(&mut self, msg: &str) {
        for s in &mut self.sessions {
            if let Some(mut ch) = s.channel.and_then(|id| s.rtc.channel(id)) {
                let _ = ch.write(false, msg.as_bytes());
            }
        }
    }
}

impl Session {
    fn event(&mut self, e: Event, choice: WebrtcCodec, commands: &mut Vec<Command>, cursor: Option<&str>) {
        match e {
            Event::MediaAdded(m) => {
                let Some(w) = self.rtc.writer(m.mid) else { return };
                let params: Vec<(Pt, RtcCodec, bool)> = w
                    .payload_params()
                    .map(|p| {
                        let (codec, f) = (p.spec().codec, p.spec().format);
                        let preferred = match codec {
                            RtcCodec::H264 => f.packetization_mode == Some(1),
                            RtcCodec::Vp9 => f.profile_id.unwrap_or(0) == 0,
                            _ => true,
                        };
                        (p.pt(), codec, preferred)
                    })
                    .collect();
                match m.kind {
                    MediaKind::Video => {
                        self.video = pick_video(choice, &params).map(|(pt, c)| (m.mid, pt, c));
                        match self.video {
                            Some((_, _, c)) => tracing::info!(codec = ?c, "WebRTC viewer video negotiated"),
                            None => tracing::warn!("the browser supports none of the enabled video codecs"),
                        }
                        if let (true, Some((_, _, c))) = (self.connected, self.video) {
                            commands.push(Command::Keyframe(c));
                        }
                    }
                    MediaKind::Audio => {
                        self.audio = params.iter().find(|p| p.1 == RtcCodec::Opus).map(|p| (m.mid, p.0));
                    }
                }
            }
            Event::Connected => {
                self.connected = true;
                tracing::info!("WebRTC viewer connected");
                if let Some((_, _, c)) = self.video {
                    commands.push(Command::Keyframe(c));
                }
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => self.rtc.disconnect(),
            Event::ChannelOpen(id, _) => {
                self.channel = Some(id);
                if let (Some(msg), Some(mut ch)) = (cursor, self.rtc.channel(id)) {
                    let _ = ch.write(false, msg.as_bytes());
                }
            }
            Event::ChannelData(d) => {
                if let Some(c) = parse_input(&d.data) {
                    commands.push(c);
                }
            }
            Event::KeyframeRequest(_) => {
                if let Some((_, _, c)) = self.video {
                    commands.push(Command::Keyframe(c));
                }
            }
            _ => {}
        }
    }
}

pub fn parse_input(data: &[u8]) -> Option<Command> {
    let v: Value = serde_json::from_slice(data).ok()?;
    let a = v.as_array()?;
    let num = |i: usize| a.get(i).and_then(Value::as_u64);
    Some(match a.first()?.as_str()? {
        "p" => Command::Pointer {
            x: num(1)?.min(u16::MAX as u64) as u16,
            y: num(2)?.min(u16::MAX as u64) as u16,
            mask: num(3)?.min(255) as u8,
        },
        "k" => Command::Key { keysym: u32::try_from(num(1)?).ok()?, down: num(2)? != 0 },
        "c" => {
            let text = a.get(1)?.as_str()?;
            if text.len() > 1 << 20 {
                return None;
            }
            Command::Clipboard(text.to_string())
        }
        "s" => Command::Resize { width: num(1)? as usize, height: num(2)? as usize },
        _ => return None,
    })
}

pub fn candidate_host(configured: Option<&str>, request_host: Option<&str>) -> Option<String> {
    let raw = configured.or(request_host)?.trim();
    let host = if let Some(rest) = raw.strip_prefix('[') {
        rest.split(']').next()?.to_string()
    } else if raw.matches(':').count() == 1 {
        raw.split(':').next()?.to_string()
    } else {
        raw.to_string()
    };
    (!host.is_empty()).then_some(host)
}

async fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .ok_or_else(|| anyhow!("{host} has no address"))
}

pub async fn signal(mut ws: WebSocket, hub: Hub, host: String) {
    let offer = match tokio::time::timeout(Duration::from_secs(15), ws.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => t,
        _ => return,
    };
    let joined = async {
        let v: Value = serde_json::from_str(offer.as_str())?;
        let sdp = v.get("sdp").and_then(Value::as_str).context("the offer has no SDP")?;
        let offer = SdpOffer::from_sdp_string(sdp).context("the offer is not valid SDP")?;
        let addr = resolve(&host, hub.port).await.with_context(|| format!("cannot resolve {host}"))?;
        hub.join(offer, addr).await
    }
    .await;
    let mut joined = match joined {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "WebRTC viewer refused");
            let msg = json!({ "type": "error", "message": format!("{e:#}") }).to_string();
            let _ = ws.send(Message::Text(msg.into())).await;
            let _ = ws.send(Message::Close(Some(CloseFrame { code: 1011, reason: "refused".into() }))).await;
            return;
        }
    };
    let answer = json!({ "type": "answer", "sdp": joined.answer.to_sdp_string() }).to_string();
    if ws.send(Message::Text(answer.into())).await.is_ok() {
        loop {
            tokio::select! {
                _ = &mut joined.closed => break,
                m = ws.recv() => match m {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                },
            }
        }
    }
    hub.leave(joined.id).await;
    let _ = ws.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_messages_become_display_commands() {
        assert!(matches!(parse_input(br#"["p", 10, 20, 1]"#), Some(Command::Pointer { x: 10, y: 20, mask: 1 })));
        assert!(matches!(parse_input(br#"["k", 65293, 1]"#), Some(Command::Key { keysym: 65293, down: true })));
        assert!(matches!(parse_input(br#"["k", 97, 0]"#), Some(Command::Key { keysym: 97, down: false })));
        assert!(matches!(parse_input(br#"["s", 1000, 1462]"#), Some(Command::Resize { width: 1000, height: 1462 })));
        assert!(matches!(parse_input(br#"["c", "hi"]"#), Some(Command::Clipboard(t)) if t == "hi"));
        assert!(parse_input(br#"["p", -1, 2, 0]"#).is_none());
        assert!(parse_input(br#"["x"]"#).is_none());
        assert!(parse_input(b"not json").is_none());
    }

    #[test]
    fn the_chosen_codec_comes_first_then_the_auto_order() {
        let all = [
            (Pt::from(96), RtcCodec::Vp8, true),
            (Pt::from(100), RtcCodec::Vp9, false),
            (Pt::from(98), RtcCodec::Vp9, true),
            (Pt::from(49), RtcCodec::H265, true),
            (Pt::from(45), RtcCodec::Av1, true),
            (Pt::from(127), RtcCodec::H264, true),
        ];
        let h264_only = [(Pt::from(125), RtcCodec::H264, false), (Pt::from(127), RtcCodec::H264, true)];
        let vp = [(Pt::from(96), RtcCodec::Vp8, true), (Pt::from(98), RtcCodec::Vp9, true)];
        assert_eq!(pick_video(WebrtcCodec::Auto, &all), Some((Pt::from(45), Codec::Av1)));
        assert_eq!(pick_video(WebrtcCodec::H264, &all), Some((Pt::from(127), Codec::H264)));
        assert_eq!(pick_video(WebrtcCodec::H265, &all), Some((Pt::from(49), Codec::H265)));
        assert_eq!(pick_video(WebrtcCodec::Vp9, &all), Some((Pt::from(98), Codec::Vp9)));
        assert_eq!(pick_video(WebrtcCodec::Vp8, &all), Some((Pt::from(96), Codec::Vp8)));
        assert_eq!(pick_video(WebrtcCodec::Auto, &h264_only), Some((Pt::from(127), Codec::H264)));
        assert_eq!(pick_video(WebrtcCodec::H265, &h264_only), Some((Pt::from(127), Codec::H264)));
        assert_eq!(pick_video(WebrtcCodec::Auto, &vp), Some((Pt::from(98), Codec::Vp9)));
        assert_eq!(pick_video(WebrtcCodec::Auto, &[]), None);
    }

    #[test]
    fn the_candidate_address_comes_from_the_dashboard_host() {
        assert_eq!(candidate_host(None, Some("203.0.113.5:6757")).as_deref(), Some("203.0.113.5"));
        assert_eq!(candidate_host(None, Some("[2001:db8::1]:6757")).as_deref(), Some("2001:db8::1"));
        assert_eq!(candidate_host(None, Some("chrome.example.com")).as_deref(), Some("chrome.example.com"));
        assert_eq!(candidate_host(Some("198.51.100.7"), Some("localhost:6757")).as_deref(), Some("198.51.100.7"));
        assert_eq!(candidate_host(None, None), None);
    }

    #[tokio::test]
    async fn names_resolve_to_an_ipv4_candidate_first() {
        let a = resolve("127.0.0.1", 6758).await.unwrap();
        assert_eq!(a, "127.0.0.1:6758".parse().unwrap());
        assert!(resolve("localhost", 1).await.is_ok());
    }
}
