use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::media::{Codec, Frame, VideoEncoder};
use super::rfb::{self, Framebuffer, Screen};
use super::{Feed, Settings};
use crate::settings::{Live, VideoQuality};

const FLUSH_FRAMES: u8 = 3;
const FLUSH_GAP: Duration = Duration::from_millis(40);
const RESTART_GAP: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum Command {
    Codecs(BTreeSet<Codec>),
    Keyframe(Codec),
    Pointer { x: u16, y: u16, mask: u8 },
    Key { keysym: u32, down: bool },
    Clipboard(String),
    Resize { width: usize, height: usize },
}

pub async fn run(settings: Arc<Settings>, mut cmds: mpsc::Receiver<Command>, out: mpsc::Sender<Feed>) {
    let mut codecs = BTreeSet::new();
    let mut live = settings.live.clone();
    loop {
        if codecs.is_empty() {
            match cmds.recv().await {
                None => return,
                Some(Command::Codecs(c)) => codecs = c,
                Some(_) => {}
            }
            continue;
        }
        match connected(&settings, &mut live, &mut codecs, &mut cmds, &out).await {
            Ok(true) => return,
            Ok(false) => continue,
            Err(e) => tracing::warn!(error = %e, "remote display capture stopped; retrying"),
        }
        let wait = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                c = cmds.recv() => match c {
                    None => return,
                    Some(Command::Codecs(c)) => codecs = c,
                    Some(_) => {}
                },
            }
        }
    }
}

struct Capture<'a> {
    settings: &'a Settings,
    out: &'a mpsc::Sender<Feed>,
    fb: Framebuffer,
    fps: u32,
    quality: VideoQuality,
    encoders: BTreeMap<Codec, VideoEncoder>,
    restarted: BTreeMap<Codec, Instant>,
    keyframes_due: BTreeMap<Codec, Instant>,
    latest: Option<Arc<Frame>>,
    painted: bool,
    dirty: bool,
    flush: u8,
    last_push: Instant,
}

impl Capture<'_> {
    fn start(&mut self, codec: Codec) {
        self.encoders.remove(&codec);
        let size = (self.fb.width, self.fb.height);
        match VideoEncoder::spawn(&self.settings.ffmpeg, codec, size, (self.fps, self.quality), self.out.clone()) {
            Ok(e) => {
                self.encoders.insert(codec, e);
                self.restarted.insert(codec, Instant::now());
                self.dirty |= self.painted;
            }
            Err(e) => tracing::error!(error = %e, ?codec, "cannot start the video encoder"),
        }
    }

    fn restart_all(&mut self) {
        let running: Vec<Codec> = self.encoders.keys().copied().collect();
        for c in running {
            self.start(c);
        }
    }

    fn sync(&mut self, codecs: &BTreeSet<Codec>) {
        self.encoders.retain(|c, _| codecs.contains(c));
        self.keyframes_due.retain(|c, _| codecs.contains(c));
        for &c in codecs {
            if !self.encoders.contains_key(&c) {
                self.start(c);
            }
        }
    }

    fn keyframe(&mut self, codec: Codec) {
        if !self.encoders.contains_key(&codec) {
            return;
        }
        let since = self.restarted.get(&codec).map(|t| t.elapsed()).unwrap_or(RESTART_GAP);
        if since >= RESTART_GAP {
            self.start(codec);
        } else {
            let due = Instant::now() + (RESTART_GAP - since);
            self.keyframes_due.entry(codec).or_insert(due);
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let push = if self.dirty {
            Some(self.last_push + Duration::from_secs(1) / self.fps)
        } else if self.flush > 0 {
            Some(self.last_push + FLUSH_GAP)
        } else {
            None
        };
        push.into_iter().chain(self.keyframes_due.values().copied()).min()
    }

    fn wake(&mut self) {
        let now = Instant::now();
        let due: Vec<Codec> = self.keyframes_due.iter().filter(|(_, t)| **t <= now).map(|(c, _)| *c).collect();
        for c in due {
            self.keyframes_due.remove(&c);
            self.start(c);
        }
        let interval = if self.dirty { Duration::from_secs(1) / self.fps } else { FLUSH_GAP };
        if !(self.dirty || self.flush > 0) || now < self.last_push + interval {
            return;
        }
        if self.dirty || self.latest.is_none() {
            self.latest =
                Some(Arc::new(Frame { width: self.fb.width, height: self.fb.height, data: self.fb.data.clone() }));
        }
        if let Some(frame) = &self.latest {
            for e in self.encoders.values() {
                e.push(frame.clone());
            }
        }
        self.last_push = now;
        if self.dirty {
            self.dirty = false;
            self.flush = FLUSH_FRAMES;
        } else {
            self.flush -= 1;
        }
    }
}

fn tuning(live: &mut watch::Receiver<Live>) -> (u32, VideoQuality) {
    let l = live.borrow_and_update();
    (l.webrtc_fps.max(1), l.webrtc_quality)
}

async fn connected(
    settings: &Settings,
    live: &mut watch::Receiver<Live>,
    codecs: &mut BTreeSet<Codec>,
    cmds: &mut mpsc::Receiver<Command>,
    out: &mpsc::Sender<Feed>,
) -> Result<bool> {
    if codecs.is_empty() {
        return Ok(false);
    }
    let mut stream = UnixStream::connect(&settings.vnc_socket).await?;
    let (width, height) = rfb::handshake(&mut stream).await?;
    let (mut rd, mut wr) = stream.into_split();
    let (fps, quality) = tuning(live);
    let mut cap = Capture {
        settings,
        out,
        fb: Framebuffer::new(width, height),
        fps,
        quality,
        encoders: BTreeMap::new(),
        restarted: BTreeMap::new(),
        keyframes_due: BTreeMap::new(),
        latest: None,
        painted: false,
        dirty: false,
        flush: 0,
        last_push: Instant::now(),
    };
    cap.sync(codecs);
    let mut screen: Option<Screen> = None;
    let mut wanted_size: Option<(usize, usize)> = None;
    let mut buf = BytesMut::with_capacity(1 << 20);
    tracing::info!(width, height, ?codecs, "streaming the remote display");
    loop {
        let wake = cap.next_wake();
        tokio::select! {
            r = rd.read_buf(&mut buf) => {
                if r? == 0 {
                    bail!("the remote display closed the connection");
                }
                while let Some((used, msg)) = rfb::parse(&buf, &mut cap.fb)? {
                    buf.advance(used);
                    if let Some(s) = msg.screen {
                        screen = Some(s);
                        if let Some((w, h)) = wanted_size.take() {
                            wr.write_all(&rfb::set_desktop_size(w, h, s)).await?;
                        }
                    }
                    if msg.resized {
                        tracing::debug!(width = cap.fb.width, height = cap.fb.height, "remote display resized");
                        cap.painted = false;
                        cap.latest = None;
                        cap.restart_all();
                        wr.write_all(&rfb::update_request(false, cap.fb.width, cap.fb.height)).await?;
                    }
                    if msg.damaged {
                        cap.painted = true;
                        cap.dirty = true;
                    }
                    if let Some(c) = msg.cursor {
                        let _ = out.try_send(Feed::Cursor(c));
                    }
                    if let Some(t) = msg.clipboard {
                        let _ = out.try_send(Feed::Clipboard(t));
                    }
                    if msg.update {
                        wr.write_all(&rfb::update_request(true, cap.fb.width, cap.fb.height)).await?;
                    }
                }
                if buf.capacity() - buf.len() < 256 * 1024 {
                    buf.reserve(4 << 20);
                }
            }
            c = cmds.recv() => match c {
                None => return Ok(true),
                Some(Command::Codecs(c)) => {
                    *codecs = c;
                    if codecs.is_empty() {
                        return Ok(false);
                    }
                    cap.sync(codecs);
                }
                Some(Command::Keyframe(codec)) => cap.keyframe(codec),
                Some(Command::Pointer { x, y, mask }) => {
                    let x = x.min(cap.fb.width.saturating_sub(1) as u16);
                    let y = y.min(cap.fb.height.saturating_sub(1) as u16);
                    wr.write_all(&rfb::pointer(mask, x, y)).await?;
                }
                Some(Command::Key { keysym, down }) => wr.write_all(&rfb::key(keysym, down)).await?,
                Some(Command::Clipboard(text)) => wr.write_all(&rfb::cut_text(&text)).await?,
                Some(Command::Resize { width, height }) => {
                    let w = (width.clamp(320, 8192)) & !1;
                    let h = (height.clamp(240, 8192)) & !1;
                    if (w, h) != (cap.fb.width, cap.fb.height) {
                        match screen {
                            Some(s) => wr.write_all(&rfb::set_desktop_size(w, h, s)).await?,
                            None => wanted_size = Some((w, h)),
                        }
                    }
                }
            },
            Ok(()) = live.changed() => {
                let tuned = tuning(live);
                if tuned != (cap.fps, cap.quality) {
                    (cap.fps, cap.quality) = tuned;
                    tracing::info!(fps = cap.fps, quality = ?cap.quality, "WebRTC video settings changed");
                    cap.restart_all();
                }
            }
            _ = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => cap.wake(),
        }
    }
}
