//! Hidden design aid: `--screenshot FILE.png` saves the window as drawn once
//! it has settled, then quits. With `--demo STATE` it captures any screen
//! without hardware. Not in `--help`; nothing here runs unless asked.

use eframe::egui;

pub struct Shot {
    path: String,
    frames: u32,
    requested: bool,
}

impl Shot {
    pub fn new(path: String) -> Self {
        Self { path, frames: 0, requested: false }
    }

    /// Call once per frame. Asks for a screenshot after a few frames (fonts
    /// and layout settled), writes it when it arrives, then closes the window.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
            for c in &image.pixels {
                rgba.extend_from_slice(&c.to_srgba_unmultiplied());
            }
            let ok = std::fs::write(&self.path, png(image.size[0], image.size[1], &rgba));
            if let Err(e) = ok {
                eprintln!("screenshot: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.frames += 1;
        if self.frames == 8 && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        ctx.request_repaint();
    }
}

/// An RGBA PNG with stored (uncompressed) deflate blocks: no codec needed.
fn png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crate::engine::crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((width * 4 + 1) * height);
    for row in rgba.chunks_exact(width * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
