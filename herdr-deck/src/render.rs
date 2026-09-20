use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    path::Path,
    sync::Arc,
};

use resvg::{tiny_skia, usvg};
use turbojpeg::{Compressor, Image, PixelFormat, Subsamp};

use herdr_client::{AgentInfo, WorkspaceInfo};

use crate::{
    config::{AgentStatus, StateColors},
    slots::agent_label,
};

/// Unique frames in a ping-pong loop (done, idle); the period is 2 * FRAMES - 2.
pub const FRAMES: u32 = 48;
/// Frames in the forward-only working loop: one sphere rotation at 20 fps.
pub const WEB_FRAMES: u32 = 316;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const JPEG_QUALITY: i32 = 88;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct RenderError(String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

struct Cache {
    images: HashMap<String, Arc<[u8]>>,
    lru: VecDeque<String>,
    bytes: usize,
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    misses: u64,
}

struct JpegEncoder {
    compressor: Compressor,
    output: Vec<u8>,
}

impl JpegEncoder {
    fn new() -> Result<Self, RenderError> {
        let mut compressor = Compressor::new()
            .map_err(|error| RenderError(format!("cannot initialize JPEG encoder: {error}")))?;
        compressor
            .set_quality(JPEG_QUALITY)
            .and_then(|()| compressor.set_subsamp(Subsamp::Sub2x2))
            .and_then(|()| compressor.set_optimize(true))
            .and_then(|()| compressor.set_progressive(false))
            .map_err(|error| RenderError(format!("cannot configure JPEG encoder: {error}")))?;
        Ok(Self {
            compressor,
            output: Vec::new(),
        })
    }

    fn encode(
        &mut self,
        rgba: &[u8],
        width: usize,
        height: usize,
        stride: usize,
    ) -> Result<Arc<[u8]>, RenderError> {
        let capacity = turbojpeg::compressed_buf_len(width, height, Subsamp::Sub2x2)
            .map_err(|error| RenderError(format!("JPEG buffer size failed: {error}")))?;
        if self.output.len() < capacity {
            self.output.resize(capacity, 0);
        }
        let size = self
            .compressor
            .compress_to_slice(
                Image {
                    pixels: rgba,
                    width,
                    pitch: stride,
                    height,
                    format: PixelFormat::RGBA,
                },
                &mut self.output,
            )
            .map_err(|error| RenderError(format!("JPEG encode failed: {error}")))?;
        Ok(Arc::from(&self.output[..size]))
    }
}

pub struct Renderer {
    svg_options: usvg::Options<'static>,
    jpeg_encoder: JpegEncoder,
    cache: Cache,
}

impl Renderer {
    pub fn new() -> Result<Self, RenderError> {
        let mut svg_options = usvg::Options::default();
        let sf = Path::new("/System/Library/Fonts/SFNS.ttf");
        let helvetica = Path::new("/System/Library/Fonts/Helvetica.ttc");
        let font = if sf.exists() { sf } else { helvetica };
        let font_data = std::fs::read(font)
            .map_err(|e| RenderError(format!("cannot load {}: {e}", font.display())))?;
        svg_options.fontdb_mut().load_font_data(font_data);
        svg_options.font_family = if sf.exists() { ".SF NS" } else { "Helvetica" }.into();
        let jpeg_encoder = JpegEncoder::new()?;
        Ok(Self {
            svg_options,
            jpeg_encoder,
            cache: Cache {
                images: HashMap::new(),
                lru: VecDeque::new(),
                bytes: 0,
                #[cfg(test)]
                hits: 0,
                #[cfg(test)]
                misses: 0,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_key(
        &mut self,
        width: u32,
        height: u32,
        agent: Option<&AgentInfo>,
        workspace: Option<&WorkspaceInfo>,
        focused: bool,
        active_session: bool,
        session_badge: Option<&str>,
        states: &HashMap<AgentStatus, StateColors>,
        offline: bool,
        time: f64,
    ) -> Result<Arc<[u8]>, RenderError> {
        let state = if offline {
            AgentStatus::Unknown
        } else {
            status(agent.map(|a| a.agent_status.as_str()).unwrap_or("unknown"))
        };
        let colors = states
            .get(&state)
            .ok_or_else(|| RenderError(format!("missing {} colors", state.as_str())))?;
        let label = agent.map(|a| agent_label(a, workspace));
        let (title, subtitle) = if offline {
            ("OFFLINE".to_owned(), String::new())
        } else if let Some(label) = label {
            (label.title, label.subtitle)
        } else {
            ("EMPTY".to_owned(), String::new())
        };
        let border = if !offline && focused {
            4.max((width as f64 / 24.0).round() as u32)
        } else {
            0
        };
        let heading = truncate(&title.to_uppercase(), 11);
        let animation = animation(state);
        let orbing = animation.shape != LoopShape::Static;
        let frame = match animation.shape {
            LoopShape::Static => 0,
            LoopShape::Forward => {
                ((time * animation.fps as f64).floor() as u64 % u64::from(animation.frames)) as u32
            }
            LoopShape::PingPong => animation_frame(time, animation.fps, animation.frames),
        };
        let key = orbing.then(|| {
            format!(
                "{width}:{height}:{}:{}:{}:{heading}:{border}:{active_session}:{frame}:{}",
                state.as_str(),
                colors.background,
                colors.foreground,
                session_badge.unwrap_or_default(),
            )
        });
        if let Some(key) = key.as_deref()
            && let Some(image) = self.cache_get(key)
        {
            return Ok(image);
        }

        // Ping-pong generators take eased time; forward loops take a phase.
        let frame_time = animation_frame_time(frame, animation.fps, animation.frames);
        let phase = f64::from(frame) / f64::from(animation.frames);
        let size = width.min(height) as f64;
        let (dots, lines) = match state {
            AgentStatus::Working => web(size, phase),
            AgentStatus::Done => (ribbon(size, frame_time * 2.34 * 0.5, false), Vec::new()),
            AgentStatus::Idle => (ribbon(size, frame_time * 3.24 * 0.25, true), Vec::new()),
            _ => (Vec::new(), Vec::new()),
        };
        let mut orb = String::new();
        for line in lines {
            let _ = write!(
                orb,
                "<line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" stroke=\"{}\" stroke-width=\"{:.2}\"/>",
                line.x1,
                line.y1,
                line.x2,
                line.y2,
                ink(line.white, line.alpha),
                line.width
            );
        }
        for dot in dots {
            let _ = write!(
                orb,
                "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" fill=\"{}\"/>",
                dot.x,
                dot.y,
                dot.r,
                ink(dot.white, dot.alpha)
            );
        }
        let family = &self.svg_options.font_family;
        let radius = (width as f64 / 10.0).round();
        let border_svg = if border == 0 {
            String::new()
        } else {
            format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\" fill=\"none\" stroke=\"#ffd60a\" stroke-width=\"{border}\"/>",
                border as f64 / 2.0,
                border as f64 / 2.0,
                width - border,
                height - border
            )
        };
        let secondary = if orbing {
            String::new()
        } else {
            format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" dominant-baseline=\"middle\" fill=\"{}\" opacity=\"0.85\" font-family=\"{}\" font-size=\"{}\">{}</text><text x=\"{}\" y=\"{}\" text-anchor=\"middle\" dominant-baseline=\"middle\" fill=\"{}\" opacity=\"0.9\" font-family=\"{}\" font-size=\"{}\" letter-spacing=\"1\">{}</text>",
                width as f64 / 2.0,
                height as f64 * 0.59,
                colors.foreground,
                family,
                (width as f64 * 0.105).round(),
                escape_xml(&truncate(&subtitle, 15)),
                width as f64 / 2.0,
                height as f64 * 0.81,
                colors.foreground,
                family,
                (width as f64 * 0.09).round(),
                state.as_str().to_uppercase()
            )
        };
        let badge = session_badge.map_or_else(String::new, |badge| {
            format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"end\" dominant-baseline=\"middle\" fill=\"{}\" opacity=\"0.8\" font-family=\"{}\" font-size=\"{}\">{}</text>",
                width as f64 * 0.94,
                height as f64 * 0.91,
                colors.foreground,
                family,
                (width as f64 * 0.075).round(),
                escape_xml(&truncate(badge, 12)),
            )
        });
        let active_badge = if active_session && !offline && agent.is_some() {
            active_badge_svg(width, height, colors)
        } else {
            String::new()
        };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><rect width=\"{width}\" height=\"{height}\" fill=\"{}\"/><rect width=\"{width}\" height=\"{height}\" rx=\"{radius}\" fill=\"{}\"/>{border_svg}{orb}<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" dominant-baseline=\"middle\" fill=\"{}\" font-family=\"{}\" font-size=\"{}\" font-weight=\"700\">{}</text>{secondary}{badge}{active_badge}</svg>",
            colors.background,
            colors.background,
            width as f64 / 2.0,
            height as f64 * if orbing { 0.14 } else { 0.36 },
            colors.foreground,
            family,
            (width as f64 * if orbing { 0.11 } else { 0.17 }).round(),
            escape_xml(&heading),
        );
        let image = self.render_svg(&svg, width, height, &colors.background)?;
        if let Some(key) = key {
            self.cache_put(key, image.clone());
        }
        Ok(image)
    }

    pub fn render_touch_strip(
        &mut self,
        width: u32,
        height: u32,
        session: &str,
        agents: &[&AgentInfo],
        states: &HashMap<AgentStatus, StateColors>,
        offline: bool,
    ) -> Result<Arc<[u8]>, RenderError> {
        let count = |name: &str| {
            agents
                .iter()
                .filter(|agent| agent.agent_status.as_str() == name)
                .count()
        };
        let section = width as f64 / 4.0;
        let working = count("working").to_string();
        let done = count("done").to_string();
        let blocked = count("blocked").to_string();
        let values = [
            (
                "SESSION",
                if offline { "OFFLINE" } else { session },
                "#282a27",
            ),
            (
                "WORK",
                working.as_str(),
                states[&AgentStatus::Working].background.as_str(),
            ),
            (
                "DONE",
                done.as_str(),
                states[&AgentStatus::Done].background.as_str(),
            ),
            (
                "BLOCK",
                blocked.as_str(),
                states[&AgentStatus::Blocked].background.as_str(),
            ),
        ];
        let family = &self.svg_options.font_family;
        let mut body = String::new();
        for (index, (label, value, color)) in values.iter().enumerate() {
            let _ = write!(
                body,
                "<g transform=\"translate({},0)\"><rect width=\"{section}\" height=\"{height}\" fill=\"{color}\"/><text x=\"{}\" y=\"{}\" text-anchor=\"middle\" fill=\"#fff\" font-family=\"{family}\" font-size=\"15\" opacity=\"0.8\">{label}</text><text x=\"{}\" y=\"{}\" text-anchor=\"middle\" fill=\"#fff\" font-family=\"{family}\" font-size=\"25\" font-weight=\"700\">{}</text></g>",
                index as f64 * section,
                section / 2.0,
                height as f64 * 0.35,
                section / 2.0,
                height as f64 * 0.75,
                escape_xml(&truncate(value, 12))
            );
        }
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\"><rect width=\"{width}\" height=\"{height}\" fill=\"#000\"/>{body}</svg>"
        );
        self.render_svg(&svg, width, height, "#000000")
    }

    #[cfg(test)]
    pub fn cache_stats(&mut self, reset: bool) -> CacheStats {
        let cache = &mut self.cache;
        let stats = CacheStats {
            hits: cache.hits,
            misses: cache.misses,
            entries: cache.images.len(),
            bytes: cache.bytes,
        };
        if reset {
            cache.hits = 0;
            cache.misses = 0;
        }
        stats
    }

    fn render_svg(
        &mut self,
        svg: &str,
        width: u32,
        height: u32,
        background: &str,
    ) -> Result<Arc<[u8]>, RenderError> {
        let tree = usvg::Tree::from_str(svg, &self.svg_options)
            .map_err(|e| RenderError(format!("invalid SVG: {e}")))?;
        let mut pixmap = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| RenderError("invalid image dimensions".into()))?;
        pixmap.fill(parse_color(background)?);
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        self.jpeg_encoder.encode(
            pixmap.data(),
            width as usize,
            height as usize,
            width as usize * 4,
        )
    }

    fn cache_get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        let cache = &mut self.cache;
        let image = cache.images.get(key)?.clone();
        #[cfg(test)]
        {
            cache.hits += 1;
        }
        Some(image)
    }

    fn cache_put(&mut self, key: String, image: Arc<[u8]>) {
        let cache = &mut self.cache;
        #[cfg(test)]
        {
            cache.misses += 1;
        }
        if let Some(old) = cache.images.insert(key.clone(), image.clone()) {
            cache.bytes -= old.len();
        }
        cache.bytes += image.len();
        cache.lru.retain(|item| item != &key);
        cache.lru.push_back(key);
        while cache.bytes > MAX_CACHE_BYTES && cache.images.len() > 1 {
            let Some(oldest) = cache.lru.pop_front() else {
                break;
            };
            if let Some(old) = cache.images.remove(&oldest) {
                cache.bytes -= old.len();
            }
        }
    }
}

fn active_badge_svg(width: u32, height: u32, colors: &StateColors) -> String {
    let size = width.min(height) as f64;
    format!(
        "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{:.2}\"/>",
        width as f64 * 0.125,
        height as f64 * 0.875,
        size * 0.05,
        colors.foreground,
        colors.background,
        size * 0.017,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopShape {
    /// One frame, no animation.
    Static,
    /// Frames play forward then backward with eased turnarounds.
    PingPong,
    /// Frames play forward and wrap; the generator must be loop-periodic.
    Forward,
}

/// How a state animates: unique frames per loop, the frame-table fps that
/// indexes them (frame `n` is rendered at `time = n / fps`), and the shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loop {
    pub frames: u32,
    pub fps: u32,
    pub shape: LoopShape,
}

pub fn animation(state: AgentStatus) -> Loop {
    let (frames, fps, shape) = match state {
        AgentStatus::Working => (WEB_FRAMES, 20, LoopShape::Forward),
        AgentStatus::Done => (FRAMES, 20, LoopShape::PingPong),
        AgentStatus::Idle => (FRAMES, 5, LoopShape::PingPong),
        AgentStatus::Blocked | AgentStatus::Unknown => (1, 1, LoopShape::Static),
    };
    Loop { frames, fps, shape }
}

fn animation_frame(time: f64, fps: u32, frames: u32) -> u32 {
    let period = u64::from(frames * 2 - 2);
    let frame = ((time * fps as f64).floor() as u64 % period) as u32;
    if frame < frames {
        frame
    } else {
        period as u32 - frame
    }
}

/// Generator time for a ping-pong frame: one loop spans `(frames - 1) / fps`
/// seconds, eased over the first and last tenth so the reversal is gentle.
pub fn animation_frame_time(frame: u32, fps: u32, frames: u32) -> f64 {
    if frames < 2 {
        return 0.0;
    }
    let edge = 0.1;
    let progress = frame as f64 / (frames - 1) as f64;
    let speed = 1.0 / (1.0 - edge);
    let eased = if progress < edge {
        speed * progress.powi(2) / (2.0 * edge)
    } else if progress > 1.0 - edge {
        1.0 - speed * (1.0 - progress).powi(2) / (2.0 * edge)
    } else {
        speed * (progress - edge / 2.0)
    };
    eased * (frames - 1) as f64 / fps as f64
}

#[derive(Clone, Copy)]
struct Dot {
    x: f64,
    y: f64,
    z: f64,
    r: f64,
    white: f64,
    alpha: f64,
}

fn projection(
    yaw: f64,
    pitch: f64,
    center: (f64, f64),
    scale: f64,
    point: (f64, f64, f64),
) -> (f64, f64, f64) {
    let (cx, cy) = center;
    let (x, y, z) = point;
    let (sp, cp) = pitch.sin_cos();
    let (sy, cyaw) = yaw.sin_cos();
    let e = x * cyaw + z * sy;
    let l = -x * sy + z * cyaw;
    let d = y * cp - l * sp;
    let depth = y * sp + l * cp;
    (cx + e * scale, cy - d * scale, depth)
}

fn hash(x: f64, y: f64) -> f64 {
    let n = (x * 12.9898 + y * 78.233).sin() * 43758.5453;
    n - n.floor()
}

fn sphere_point(index: usize, count: usize) -> (f64, f64, f64) {
    let golden = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    let y = 1.0 - 2.0 * (index as f64 + 0.5) / count as f64;
    let radius = (1.0 - y * y).sqrt();
    let angle = index as f64 * golden;
    (radius * angle.cos(), y, radius * angle.sin())
}

fn finalize(mut dots: Vec<Dot>, min_radius: f64) -> Vec<Dot> {
    dots.retain(|d| d.alpha >= 0.02);
    for dot in &mut dots {
        dot.r = dot.r.max(min_radius);
    }
    dots.sort_by(|a, b| a.z.total_cmp(&b.z));
    dots
}

struct Line {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    white: f64,
    alpha: f64,
    width: f64,
}

fn noise2(x: f64, y: f64) -> f64 {
    let (xi, yi) = (x.floor(), y.floor());
    let mut fx = x - xi;
    let mut fy = y - yi;
    fx = fx * fx * (3.0 - 2.0 * fx);
    fy = fy * fy * (3.0 - 2.0 * fy);
    let a = hash(xi, yi);
    let b = hash(xi + 1.0, yi);
    let c = hash(xi, yi + 1.0);
    let d = hash(xi + 1.0, yi + 1.0);
    a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy
}

// Every animated rate is an exact integer number of cycles per loop (yaw one
// turn, pulse 12 cycles, signals 29 hops, drift sampled on closed noise-space
// circles), so frame WEB_FRAMES - 1 flows seamlessly into frame 0.
fn web(size: f64, phase: f64) -> (Vec<Dot>, Vec<Line>) {
    const NODES: usize = 41;
    const THRESHOLD: f64 = 0.72;
    const SIGNALS: usize = 7;
    const NODE_R: f64 = 1.33;
    const NODE_R_DEPTH: f64 = 1.71;
    const SIGNAL_HOPS: f64 = 29.0;

    let angle = phase * std::f64::consts::TAU;
    let center = size / 2.0;
    let radius = size / 2.0 * 0.8;
    let scale = (size / 300.0).powf(0.6);
    let project = |point: (f64, f64, f64)| projection(angle, 0.32, (center, center), radius, point);
    let drift = |offset: f64, rate: f64| {
        let r = rate / 0.12;
        noise2(offset + r * angle.cos(), r * angle.sin())
    };
    let mut points = Vec::with_capacity(NODES);
    for index in 0..NODES {
        let i = index as f64;
        let (bx, by, bz) = sphere_point(index, NODES);
        let x = bx + 0.3 * (drift(i * 0.31 + 9.0, 0.24) - 0.5) * 2.0;
        let y = by + 0.3 * (drift(i * 0.53 + 27.0, 0.21) - 0.5) * 2.0;
        let z = bz + 0.3 * (drift(i * 0.77 + 55.0, 0.27) - 0.5) * 2.0;
        let norm = (x * x + y * y + z * z).sqrt();
        points.push((x / norm, y / norm, z / norm));
    }
    let mut lines = Vec::new();
    for i in 0..NODES {
        for j in i + 1..NODES {
            let dx = points[i].0 - points[j].0;
            let dy = points[i].1 - points[j].1;
            let dz = points[i].2 - points[j].2;
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            if distance >= THRESHOLD {
                continue;
            }
            let (x1, y1, z1) = project(points[i]);
            let (x2, y2, z2) = project(points[j]);
            let d = ((z1 + z2) / 2.0 + 1.0) / 2.0;
            let alpha = (1.0 - distance / THRESHOLD) * (0.3 + 0.55 * d);
            if alpha < 0.02 {
                continue;
            }
            lines.push(Line {
                x1,
                y1,
                x2,
                y2,
                white: 0.42,
                alpha,
                width: (0.8 * scale).max(0.6),
            });
        }
    }
    let mut dots = Vec::new();
    for (index, &point) in points.iter().enumerate() {
        let (px, py, depth) = project(point);
        let d = (depth + 1.0) / 2.0;
        let pulse = 1.0 + 0.25 * (12.0 * angle + index as f64 * 2.7).sin();
        dots.push(Dot {
            x: px,
            y: py,
            z: depth,
            r: (NODE_R + NODE_R_DEPTH * d) * pulse * scale,
            white: 0.55 - 0.45 * d,
            alpha: 1.0,
        });
    }
    for signal in 0..SIGNALS {
        let s = signal as f64;
        let travel = phase * SIGNAL_HOPS + s * 7.31;
        let step = travel.floor();
        let hop = step.rem_euclid(SIGNAL_HOPS);
        let from = ((hash(hop, s * 3.1 + 1.7) * NODES as f64).floor() as usize).min(NODES - 1);
        let to = ((hash(hop, s * 5.7 + 4.2) * NODES as f64).floor() as usize).min(NODES - 1);
        if from == to {
            continue;
        }
        let progress = travel - step;
        let x = points[from].0 + (points[to].0 - points[from].0) * progress;
        let y = points[from].1 + (points[to].1 - points[from].1) * progress;
        let z = points[from].2 + (points[to].2 - points[from].2) * progress;
        let norm = (x * x + y * y + z * z).sqrt().max(1e-6);
        let (px, py, depth) = project((x / norm, y / norm, z / norm));
        let d = (depth + 1.0) / 2.0;
        dots.push(Dot {
            x: px,
            y: py,
            z: depth,
            r: (NODE_R * 1.5 + NODE_R_DEPTH * d) * scale,
            white: 0.05,
            alpha: 0.5 + 0.5 * d,
        });
    }
    (finalize(dots, 0.3), lines)
}

fn ribbon(size: f64, time: f64, ring: bool) -> Vec<Dot> {
    let center = size / 2.0;
    let radius = size / 2.0 * 0.78;
    let (base_lanes, segs, ghosts, r_base, r_depth, band, wob, face_on) = if ring {
        (3, 44, 0, 1.1 * 0.956, 1.7 * 0.956, 3.627, 0.368, true)
    } else {
        (3, 44, 38, 1.1 * 0.85, 1.7 * 0.85, 3.9, 1.0, false)
    };
    let lanes = (base_lanes as f64 * band).round().max(1.0) as usize;
    let scale = (size / 300.0).powf(0.6);
    let spin = 0.0;
    let mut dots = Vec::new();
    for i in 0..ghosts {
        let (x, y, z) = sphere_point(i, ghosts);
        let (px, py, depth) = projection(
            time * 0.1 * spin,
            0.3,
            (center, center),
            1.0,
            (x * radius, y * radius, z * radius),
        );
        let d = (depth / radius + 1.0) / 2.0;
        dots.push(Dot {
            x: px,
            y: py,
            z: depth,
            r: 0.8 * scale,
            white: 0.78,
            alpha: 0.1 + 0.22 * d,
        });
    }
    let e = time * 0.24 * spin;
    let tilt = if face_on {
        -0.3
    } else {
        0.55 + 0.3 * (time * 0.18).sin() * spin
    };
    let (se, ce) = e.sin_cos();
    let (st, ct) = tilt.sin_cos();
    let (dx, dy, dz) = (ce, 0.0, se);
    let (ux, uy, uz) = (-se * st, ct, ce * st);
    let (vx, vy, vz) = (dy * uz - dz * uy, dz * ux - dx * uz, dx * uy - dy * ux);
    let x_wob = 0.23 * wob;
    let adjusted = if face_on {
        radius / (1.0 + 0.85 * x_wob)
    } else {
        radius
    };
    for lane in 0..lanes {
        let offset = (lane as f64 - (lanes - 1) as f64 / 2.0) * 0.075;
        let edge =
            (lane as f64 - (lanes - 1) as f64 / 2.0).abs() / ((lanes - 1) as f64 / 2.0).max(1.0);
        for segment in 0..segs {
            let angle = segment as f64 / segs as f64 * std::f64::consts::TAU;
            let wave = (0.16 * (angle * 3.0 - time * 1.7 + lane as f64 * 0.22).sin()
                + 0.07 * (angle * 5.0 + time * 1.1).sin())
                * wob;
            let radial = if face_on { 1.0 + wave } else { 1.0 };
            let cross = if face_on { offset } else { offset + wave };
            let x = dx * angle.cos() + ux * angle.sin() + vx * cross;
            let y = dy * angle.cos() + uy * angle.sin() + vy * cross;
            let z = dz * angle.cos() + uz * angle.sin() + vz * cross;
            let norm = (x * x + y * y + z * z).sqrt();
            let (px, py, depth) = projection(
                time * 0.1 * spin,
                0.3,
                (center, center),
                1.0,
                (
                    x / norm * adjusted * radial,
                    y / norm * adjusted * radial,
                    z / norm * adjusted * radial,
                ),
            );
            let d = (depth / radius + 1.0) / 2.0;
            dots.push(Dot {
                x: px,
                y: py,
                z: depth,
                r: (r_base + r_depth * d) * (1.0 - 0.25 * edge) * scale,
                white: 0.52 - 0.44 * d + 0.18 * edge,
                alpha: 0.4 + 0.6 * d,
            });
        }
    }
    finalize(dots, 0.3)
}

fn status(value: &str) -> AgentStatus {
    match value {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "done" => AgentStatus::Done,
        "blocked" => AgentStatus::Blocked,
        _ => AgentStatus::Unknown,
    }
}
fn truncate(value: &str, length: usize) -> String {
    let count = value.chars().count();
    if count <= length {
        value.into()
    } else {
        value
            .chars()
            .take(length.saturating_sub(1).max(1))
            .collect::<String>()
            + "…"
    }
}
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn ink(white: f64, alpha: f64) -> String {
    let gray = ((1.0 - white.clamp(0.0, 1.0)) * 255.0).round() as u8;
    if alpha < 1.0 {
        format!("rgba({gray},{gray},{gray},{:.3})", alpha)
    } else {
        format!("rgb({gray},{gray},{gray})")
    }
}
fn parse_color(value: &str) -> Result<tiny_skia::Color, RenderError> {
    let s = value
        .strip_prefix('#')
        .ok_or_else(|| RenderError(format!("unsupported color {value}")))?;
    let n =
        u32::from_str_radix(s, 16).map_err(|_| RenderError(format!("invalid color {value}")))?;
    let (r, g, b) = match s.len() {
        3 => (((n >> 8) & 15) * 17, ((n >> 4) & 15) * 17, (n & 15) * 17),
        6 => ((n >> 16) & 255, (n >> 8) & 255, n & 255),
        _ => return Err(RenderError(format!("invalid color {value}"))),
    };
    Ok(tiny_skia::Color::from_rgba8(r as u8, g as u8, b as u8, 255))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> HashMap<AgentStatus, StateColors> {
        AgentStatus::ALL
            .into_iter()
            .map(|status| {
                (
                    status,
                    StateColors {
                        background: "#111111".into(),
                        foreground: "#eeeeee".into(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn frame_time_eases_gently_at_turnaround() {
        let a = animation_frame_time(46, 20, FRAMES);
        let b = animation_frame_time(47, 20, FRAMES);
        assert!(b > a);
        assert!(
            b - a < animation_frame_time(24, 20, FRAMES) - animation_frame_time(23, 20, FRAMES)
        );
        assert_eq!(animation_frame(47.0 / 20.0, 20, FRAMES), 47);
        assert_eq!(animation_frame(48.0 / 20.0, 20, FRAMES), 46);
    }

    #[test]
    fn web_orb_loops_seamlessly() {
        let (start_dots, start_lines) = web(240.0, 0.0);
        let (end_dots, end_lines) = web(240.0, 1.0);
        assert_eq!(start_dots.len(), end_dots.len());
        assert_eq!(start_lines.len(), end_lines.len());
        for (a, b) in start_dots.iter().zip(&end_dots) {
            assert!((a.x - b.x).abs() < 1e-6);
            assert!((a.y - b.y).abs() < 1e-6);
            assert!((a.r - b.r).abs() < 1e-6);
            assert!((a.alpha - b.alpha).abs() < 1e-6);
        }
    }

    #[test]
    fn animation_advances_at_current_epoch_times() {
        let now = 1_787_000_000.0;
        assert_ne!(
            animation_frame(now, 20, FRAMES),
            animation_frame(now + 0.05, 20, FRAMES)
        );
    }

    #[test]
    fn active_badge_uses_the_bottom_left_corner() {
        let colors = &palette()[&AgentStatus::Working];
        assert_eq!(
            active_badge_svg(120, 120, colors),
            "<circle cx=\"15.00\" cy=\"105.00\" r=\"6.00\" fill=\"#eeeeee\" stroke=\"#111111\" stroke-width=\"2.04\"/>"
        );
    }

    #[test]
    fn animated_jpeg_is_cached_at_device_dimensions() {
        let mut renderer = Renderer::new().unwrap();
        let agent = AgentInfo {
            terminal_id: "t1".into(),
            name: Some("coder".into()),
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: "working".into(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            workspace_id: "w1".into(),
            tab_id: "tab1".into(),
            pane_id: "p1".into(),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq: 0,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        };
        let first = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                None,
                false,
                false,
                Some("local/default"),
                &palette(),
                false,
                1.0,
            )
            .unwrap();
        let second = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                None,
                false,
                false,
                Some("local/default"),
                &palette(),
                false,
                1.0,
            )
            .unwrap();
        let third = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                None,
                false,
                false,
                Some("local/default"),
                &palette(),
                false,
                1.05,
            )
            .unwrap();
        let other_badge = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                None,
                false,
                false,
                Some("review"),
                &palette(),
                false,
                1.0,
            )
            .unwrap();
        let active = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                None,
                false,
                true,
                Some("local/default"),
                &palette(),
                false,
                1.0,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_ne!(first.as_ref(), third.as_ref());
        assert_ne!(first.as_ref(), other_badge.as_ref());
        assert_ne!(first.as_ref(), active.as_ref());
        let sof = first
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xc0])
            .expect("baseline SOF marker");
        assert_eq!(&first[sof + 2..sof + 4], &[0, 17]);
        assert_eq!(&first[sof + 5..sof + 7], &120_u16.to_be_bytes());
        assert_eq!(&first[sof + 7..sof + 9], &120_u16.to_be_bytes());
        assert_eq!(
            [first[sof + 11], first[sof + 14], first[sof + 17]],
            [0x22, 0x11, 0x11]
        );
        assert_eq!(
            [first[sof + 12], first[sof + 15], first[sof + 18]],
            [0, 1, 1]
        );
        assert_eq!(&first[..11], b"\xff\xd8\xff\xe0\0\x10JFIF\0");
        assert!(!first.windows(2).any(|bytes| bytes == [0xff, 0xc1]));
        assert!(!first.windows(2).any(|bytes| bytes == [0xff, 0xc2]));
        assert_eq!(
            first
                .windows(2)
                .filter(|bytes| *bytes == [0xff, 0xda])
                .count(),
            1
        );
        assert_eq!(renderer.cache_stats(false).hits, 1);
    }
}
