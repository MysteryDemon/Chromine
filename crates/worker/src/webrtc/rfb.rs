use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const ENC_RAW: i32 = 0;
const ENC_CURSOR: i32 = -239;
const ENC_DESKTOP_SIZE: i32 = -223;
const ENC_EXT_DESKTOP_SIZE: i32 = -308;
const MAX_SIDE: usize = 8192;
const MAX_CURSOR: usize = 512;
const MAX_CLIPBOARD: usize = 4 << 20;

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Framebuffer {
        Framebuffer { width, height, data: vec![0; width * height * 4] }
    }

    fn resize(&mut self, width: usize, height: usize) {
        *self = Framebuffer::new(width, height);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub hot_x: u16,
    pub hot_y: u16,
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub id: u32,
    pub flags: u32,
}

#[derive(Debug, Default, PartialEq)]
pub struct Message {
    pub update: bool,
    pub damaged: bool,
    pub resized: bool,
    pub screen: Option<Screen>,
    pub cursor: Option<Cursor>,
    pub clipboard: Option<String>,
}

pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> Result<(usize, usize)> {
    let mut version = [0u8; 12];
    s.read_exact(&mut version).await?;
    if &version[..4] != b"RFB " {
        bail!("the display socket did not answer with the VNC protocol");
    }
    s.write_all(b"RFB 003.008\n").await?;
    let count = s.read_u8().await? as usize;
    if count == 0 {
        bail!("the display refused the connection");
    }
    let mut types = vec![0u8; count];
    s.read_exact(&mut types).await?;
    if !types.contains(&1) {
        bail!("the display requires a password");
    }
    s.write_u8(1).await?;
    if s.read_u32().await? != 0 {
        bail!("the display rejected the connection");
    }
    s.write_u8(1).await?;
    let width = s.read_u16().await? as usize;
    let height = s.read_u16().await? as usize;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        bail!("the display reported an impossible size {width}x{height}");
    }
    let mut format = [0u8; 16];
    s.read_exact(&mut format).await?;
    let name_len = s.read_u32().await? as u64;
    if name_len > 4096 {
        bail!("the display sent an oversized desktop name");
    }
    tokio::io::copy(&mut (&mut *s).take(name_len), &mut tokio::io::sink()).await?;
    s.write_all(&set_pixel_format()).await?;
    s.write_all(&set_encodings(&[ENC_RAW, ENC_CURSOR, ENC_EXT_DESKTOP_SIZE, ENC_DESKTOP_SIZE])).await?;
    s.write_all(&update_request(false, width, height)).await?;
    Ok((width, height))
}

fn set_pixel_format() -> Vec<u8> {
    let mut m = vec![0u8, 0, 0, 0];
    m.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    m
}

fn set_encodings(encodings: &[i32]) -> Vec<u8> {
    let mut m = vec![2u8, 0];
    m.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        m.extend_from_slice(&e.to_be_bytes());
    }
    m
}

pub fn update_request(incremental: bool, width: usize, height: usize) -> Vec<u8> {
    let mut m = vec![3u8, incremental as u8, 0, 0, 0, 0];
    m.extend_from_slice(&(width as u16).to_be_bytes());
    m.extend_from_slice(&(height as u16).to_be_bytes());
    m
}

pub fn key(keysym: u32, down: bool) -> Vec<u8> {
    let mut m = vec![4u8, down as u8, 0, 0];
    m.extend_from_slice(&keysym.to_be_bytes());
    m
}

pub fn pointer(mask: u8, x: u16, y: u16) -> Vec<u8> {
    let mut m = vec![5u8, mask];
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m
}

pub fn cut_text(text: &str) -> Vec<u8> {
    let latin1: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    let mut m = vec![6u8, 0, 0, 0];
    m.extend_from_slice(&(latin1.len() as u32).to_be_bytes());
    m.extend_from_slice(&latin1);
    m
}

pub fn set_desktop_size(width: usize, height: usize, screen: Screen) -> Vec<u8> {
    let mut m = vec![251u8, 0];
    m.extend_from_slice(&(width as u16).to_be_bytes());
    m.extend_from_slice(&(height as u16).to_be_bytes());
    m.extend_from_slice(&[1, 0]);
    m.extend_from_slice(&screen.id.to_be_bytes());
    m.extend_from_slice(&[0, 0, 0, 0]);
    m.extend_from_slice(&(width as u16).to_be_bytes());
    m.extend_from_slice(&(height as u16).to_be_bytes());
    m.extend_from_slice(&screen.flags.to_be_bytes());
    m
}

fn u16_at(b: &[u8], i: usize) -> usize {
    u16::from_be_bytes([b[i], b[i + 1]]) as usize
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn rect_len(buf: &[u8], at: usize, enc: i32, w: usize, h: usize) -> Result<Option<usize>> {
    Ok(Some(match enc {
        ENC_RAW => w * h * 4,
        ENC_CURSOR => w * h * 4 + w.div_ceil(8) * h,
        ENC_DESKTOP_SIZE => 0,
        ENC_EXT_DESKTOP_SIZE => {
            if buf.len() < at + 4 {
                return Ok(None);
            }
            4 + buf[at] as usize * 16
        }
        other => bail!("the display sent an unrequested encoding {other}"),
    }))
}

fn update_len(buf: &[u8], mut width: usize, mut height: usize) -> Result<Option<usize>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let mut at = 4;
    for _ in 0..u16_at(buf, 2) {
        if buf.len() < at + 12 {
            return Ok(None);
        }
        let (x, y) = (u16_at(buf, at), u16_at(buf, at + 2));
        let (w, h) = (u16_at(buf, at + 4), u16_at(buf, at + 6));
        let enc = u32_at(buf, at + 8) as i32;
        at += 12;
        match enc {
            ENC_RAW if x + w > width || y + h > height => bail!("the display sent a rectangle outside the screen"),
            ENC_CURSOR if w > MAX_CURSOR || h > MAX_CURSOR => bail!("the display sent an oversized cursor"),
            ENC_DESKTOP_SIZE | ENC_EXT_DESKTOP_SIZE if enc == ENC_DESKTOP_SIZE || y == 0 => {
                if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                    bail!("the display reported an impossible size {w}x{h}");
                }
                (width, height) = (w, h);
            }
            _ => {}
        }
        match rect_len(buf, at, enc, w, h)? {
            Some(n) => at += n,
            None => return Ok(None),
        }
    }
    Ok((buf.len() >= at).then_some(at))
}

fn apply_update(buf: &[u8], fb: &mut Framebuffer, msg: &mut Message) -> Result<()> {
    let mut at = 4;
    for _ in 0..u16_at(buf, 2) {
        let (x, y) = (u16_at(buf, at), u16_at(buf, at + 2));
        let (w, h) = (u16_at(buf, at + 4), u16_at(buf, at + 6));
        let enc = u32_at(buf, at + 8) as i32;
        at += 12;
        match enc {
            ENC_RAW => {
                for row in 0..h {
                    let src = at + row * w * 4;
                    let dst = ((y + row) * fb.width + x) * 4;
                    fb.data[dst..dst + w * 4].copy_from_slice(&buf[src..src + w * 4]);
                }
                if w > 0 && h > 0 {
                    msg.damaged = true;
                }
            }
            ENC_CURSOR => {
                let mask_row = w.div_ceil(8);
                let mut rgba = vec![0u8; w * h * 4];
                for py in 0..h {
                    for px in 0..w {
                        let p = at + (py * w + px) * 4;
                        let visible = buf[at + w * h * 4 + py * mask_row + px / 8] & (0x80 >> (px % 8)) != 0;
                        let o = (py * w + px) * 4;
                        rgba[o..o + 4].copy_from_slice(&[
                            buf[p + 2],
                            buf[p + 1],
                            buf[p],
                            if visible { 255 } else { 0 },
                        ]);
                    }
                }
                msg.cursor = Some(Cursor { hot_x: x as u16, hot_y: y as u16, width: w as u16, height: h as u16, rgba });
            }
            ENC_DESKTOP_SIZE | ENC_EXT_DESKTOP_SIZE => {
                if enc == ENC_EXT_DESKTOP_SIZE && buf[at] > 0 {
                    msg.screen = Some(Screen { id: u32_at(buf, at + 4), flags: u32_at(buf, at + 16) });
                }
                if (enc == ENC_DESKTOP_SIZE || y == 0) && (w, h) != (fb.width, fb.height) {
                    fb.resize(w, h);
                    msg.resized = true;
                }
            }
            _ => unreachable!(),
        }
        at += rect_len(buf, at, enc, w, h)?.unwrap_or(0);
    }
    msg.update = true;
    Ok(())
}

pub fn parse(buf: &[u8], fb: &mut Framebuffer) -> Result<Option<(usize, Message)>> {
    let Some(&kind) = buf.first() else { return Ok(None) };
    let mut msg = Message::default();
    let used = match kind {
        0 => {
            let Some(n) = update_len(buf, fb.width, fb.height)? else { return Ok(None) };
            apply_update(&buf[..n], fb, &mut msg)?;
            n
        }
        1 => {
            if buf.len() < 6 {
                return Ok(None);
            }
            let n = 6 + u16_at(buf, 4) * 6;
            if buf.len() < n {
                return Ok(None);
            }
            n
        }
        2 => 1,
        3 => {
            if buf.len() < 8 {
                return Ok(None);
            }
            let len = u32_at(buf, 4) as i32;
            if len.unsigned_abs() as usize > MAX_CLIPBOARD {
                bail!("the display sent an oversized clipboard");
            }
            let n = 8 + len.unsigned_abs() as usize;
            if buf.len() < n {
                return Ok(None);
            }
            if len >= 0 {
                msg.clipboard = Some(buf[8..n].iter().map(|&b| b as char).collect());
            }
            n
        }
        other => bail!("the display sent an unknown message type {other}"),
    };
    Ok(Some((used, msg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16, enc: i32) -> Vec<u8> {
        let mut r = Vec::new();
        for v in [x, y, w, h] {
            r.extend_from_slice(&v.to_be_bytes());
        }
        r.extend_from_slice(&enc.to_be_bytes());
        r
    }

    fn update(rects: &[Vec<u8>]) -> Vec<u8> {
        let mut m = vec![0u8, 0];
        m.extend_from_slice(&(rects.len() as u16).to_be_bytes());
        for r in rects {
            m.extend_from_slice(r);
        }
        m
    }

    #[test]
    fn raw_rectangles_land_in_the_framebuffer() {
        let mut fb = Framebuffer::new(4, 3);
        let mut r = rect(1, 1, 2, 2, ENC_RAW);
        r.extend((0..16).map(|i| i as u8));
        let msg = update(&[r]);
        for cut in 0..msg.len() {
            assert!(parse(&msg[..cut], &mut fb).unwrap().is_none(), "partial message at {cut} must wait");
        }
        let (used, m) = parse(&msg, &mut fb).unwrap().unwrap();
        assert_eq!(used, msg.len());
        assert!(m.update && m.damaged && !m.resized);
        assert_eq!(&fb.data[(4 + 1) * 4..(4 + 1) * 4 + 8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(&fb.data[(2 * 4 + 1) * 4..(2 * 4 + 1) * 4 + 8], &[8, 9, 10, 11, 12, 13, 14, 15]);
        assert!(fb.data[..4].iter().all(|&b| b == 0));
    }

    #[test]
    fn cursor_and_screen_size_are_reported() {
        let mut fb = Framebuffer::new(8, 8);
        let mut cur = rect(1, 2, 2, 1, ENC_CURSOR);
        cur.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0, 0b1000_0000]);
        let mut ext = rect(0, 0, 640, 480, ENC_EXT_DESKTOP_SIZE);
        ext.extend_from_slice(&[1, 0, 0, 0]);
        ext.extend_from_slice(&7u32.to_be_bytes());
        ext.extend_from_slice(&[0, 0, 0, 0, 2, 128, 1, 224]);
        ext.extend_from_slice(&9u32.to_be_bytes());
        let (_, m) = parse(&update(&[cur, ext]), &mut fb).unwrap().unwrap();
        let c = m.cursor.unwrap();
        assert_eq!((c.hot_x, c.hot_y, c.width, c.height), (1, 2, 2, 1));
        assert_eq!(c.rgba, vec![30, 20, 10, 255, 60, 50, 40, 0]);
        assert_eq!(m.screen, Some(Screen { id: 7, flags: 9 }));
        assert!(m.resized && !m.damaged);
        assert_eq!((fb.width, fb.height, fb.data.len()), (640, 480, 640 * 480 * 4));
    }

    #[test]
    fn a_failed_resize_from_another_client_keeps_the_size() {
        let mut fb = Framebuffer::new(8, 8);
        let mut ext = rect(1, 3, 640, 480, ENC_EXT_DESKTOP_SIZE);
        ext.extend_from_slice(&[0, 0, 0, 0]);
        let (_, m) = parse(&update(&[ext]), &mut fb).unwrap().unwrap();
        assert!(!m.resized);
        assert_eq!((fb.width, fb.height), (8, 8));
    }

    #[test]
    fn clipboard_bell_and_colour_maps() {
        let mut fb = Framebuffer::new(1, 1);
        let mut cut = vec![3u8, 0, 0, 0, 0, 0, 0, 3];
        cut.extend_from_slice(&[b'h', 0xe9, b'!']);
        assert_eq!(parse(&cut, &mut fb).unwrap().unwrap().1.clipboard.as_deref(), Some("hé!"));
        assert_eq!(parse(&[2], &mut fb).unwrap().unwrap().0, 1);
        assert_eq!(parse(&[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0], &mut fb).unwrap().unwrap().0, 12);
        assert!(parse(&[9], &mut fb).is_err());
    }

    #[test]
    fn out_of_bounds_and_unknown_encodings_are_rejected() {
        let mut fb = Framebuffer::new(2, 2);
        let mut r = rect(1, 1, 2, 2, ENC_RAW);
        r.extend(std::iter::repeat_n(0, 16));
        assert!(parse(&update(&[r]), &mut fb).is_err());
        assert!(parse(&update(&[rect(0, 0, 1, 1, 7)]), &mut fb).is_err());
    }

    #[test]
    fn oversized_claims_are_refused_from_the_header_alone() {
        let mut fb = Framebuffer::new(64, 64);
        assert!(parse(&update(&[rect(0, 0, 600, 600, ENC_CURSOR)]), &mut fb).is_err());
        assert!(parse(&update(&[rect(0, 0, 65, 1, ENC_RAW)]), &mut fb).is_err());
        assert!(parse(&update(&[rect(0, 0, 9000, 9000, ENC_DESKTOP_SIZE)]), &mut fb).is_err());
        assert!(parse(&[3, 0, 0, 0, 0x7f, 0xff, 0xff, 0xff], &mut fb).is_err());
        let grow = update(&[rect(0, 0, 128, 128, ENC_DESKTOP_SIZE), rect(100, 100, 28, 28, ENC_RAW)]);
        assert!(parse(&grow, &mut fb).unwrap().is_none(), "a rectangle inside the new size waits for its pixels");
    }

    #[test]
    fn client_messages_match_the_protocol() {
        assert_eq!(pointer(5, 300, 2), vec![5, 5, 1, 44, 0, 2]);
        assert_eq!(key(0xff0d, true), vec![4, 1, 0, 0, 0, 0, 0xff, 0x0d]);
        assert_eq!(cut_text("aé€"), vec![6, 0, 0, 0, 0, 0, 0, 3, b'a', 0xe9, b'?']);
        assert_eq!(update_request(true, 1600, 900), vec![3, 1, 0, 0, 0, 0, 6, 64, 3, 132]);
        let d = set_desktop_size(1000, 1462, Screen { id: 3, flags: 0 });
        assert_eq!(d.len(), 24);
        assert_eq!(&d[..8], &[251, 0, 3, 232, 5, 182, 1, 0]);
    }

    #[tokio::test]
    async fn handshake_negotiates_raw_true_colour() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(async move {
            server.write_all(b"RFB 003.008\n").await.unwrap();
            let mut v = [0u8; 12];
            server.read_exact(&mut v).await.unwrap();
            server.write_all(&[1, 1]).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 1);
            server.write_all(&0u32.to_be_bytes()).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 1);
            let mut init = vec![6, 64, 3, 132];
            init.extend_from_slice(&[0; 16]);
            init.extend_from_slice(&4u32.to_be_bytes());
            init.extend_from_slice(b"test");
            server.write_all(&init).await.unwrap();
            let mut rest = vec![0u8; 20 + 4 + 16 + 10];
            server.read_exact(&mut rest).await.unwrap();
            rest
        });
        assert_eq!(handshake(&mut client).await.unwrap(), (1600, 900));
        let rest = srv.await.unwrap();
        assert_eq!(&rest[..8], &[0, 0, 0, 0, 32, 24, 0, 1]);
        assert_eq!(&rest[20..24], &[2, 0, 0, 4]);
        assert_eq!(&rest[40..], &update_request(false, 1600, 900)[..]);
    }
}
