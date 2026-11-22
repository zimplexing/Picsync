//! The Picsync brand mark.
//!
//! A single renderer draws the logo at any size: the window icon, the in-app
//! header mark and the Windows executable icon all come from this code, so the
//! artwork can never drift between them. Only `std` is used so the build script
//! can include this file directly and produce the `.ico` without extra crates.
//!
//! The mark is a teal tile holding a white instant-photo frame: a teal scene
//! with a white sun and two mountain ridges, plus a thin caption bar on the
//! card's lower lip. Small sizes drop the lip and the back ridge so the mark
//! stays legible at 16 px.

/// Icon sizes embedded into the Windows executable.
pub const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

const TILE_TOP: [f32; 3] = [9.0, 74.0, 67.0];
const TILE_BOTTOM: [f32; 3] = [17.0, 163.0, 144.0];
const PHOTO_TOP: [f32; 3] = [16.0, 131.0, 118.0];
const PHOTO_BOTTOM: [f32; 3] = [35.0, 181.0, 161.0];
const WHITE: [f32; 3] = [255.0, 255.0, 255.0];

/// Render the mark as straight (non-premultiplied) RGBA8 rows, top to bottom.
pub fn render(size: u32) -> Vec<u8> {
    assert!(size > 0, "icon size must be positive");
    let mark = Mark::new(size as f32, size >= 32);
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    const SAMPLES: u32 = 4;
    let step = 1.0 / SAMPLES as f32;
    let inv_samples = 1.0 / (SAMPLES * SAMPLES) as f32;
    for py in 0..size {
        for px in 0..size {
            let mut covered = 0.0f32;
            let mut color = [0.0f32; 3];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) * step;
                    let y = py as f32 + (sy as f32 + 0.5) * step;
                    if let Some(rgb) = mark.shade(x, y) {
                        covered += 1.0;
                        color[0] += rgb[0];
                        color[1] += rgb[1];
                        color[2] += rgb[2];
                    }
                }
            }
            let index = ((py * size + px) * 4) as usize;
            if covered > 0.0 {
                for (channel, value) in color.iter().enumerate() {
                    rgba[index + channel] = (value / covered).round().clamp(0.0, 255.0) as u8;
                }
            }
            rgba[index + 3] = (covered * inv_samples * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    rgba
}

/// Geometry of one mark at a given pixel size.
struct Mark {
    size: f32,
    tile_radius: f32,
    /// White instant-photo frame. Omitted at tiny sizes, where such a frame
    /// would swallow the scene and turn the mark into a white blob.
    frame: Option<(Box2, f32)>,
    /// The photo opening inside the frame, or the scene bounds when there is no
    /// frame: sun and ridges are laid out relative to this box.
    scene: Box2,
    scene_radius: f32,
    sun: (f32, f32),
    sun_radius: f32,
    back_ridge: bool,
    caption: Option<(Box2, f32)>,
}

impl Mark {
    fn new(size: f32, detailed: bool) -> Self {
        if detailed {
            let frame = Box2::new(size * 0.095, size * 0.09, size * 0.905, size * 0.90);
            let scene = Box2::new(
                frame.left + size * 0.042,
                frame.top + size * 0.042,
                frame.right - size * 0.042,
                frame.top + frame.height() * 0.795,
            );
            let height = (size * 0.030).max(1.0);
            let width = size * 0.255;
            let center_x = (frame.left + frame.right) * 0.5;
            let center_y = (scene.bottom + frame.bottom) * 0.5;
            let caption = Box2::new(
                center_x - width * 0.5,
                center_y - height * 0.5,
                center_x + width * 0.5,
                center_y + height * 0.5,
            );
            Self {
                size,
                tile_radius: size * 0.235,
                frame: Some((frame, size * 0.052)),
                sun: (
                    scene.left + scene.width() * 0.30,
                    scene.top + scene.height() * 0.33,
                ),
                sun_radius: scene.width() * 0.115,
                scene,
                scene_radius: size * 0.030,
                back_ridge: true,
                caption: Some((caption, height * 0.5)),
            }
        } else {
            let scene = Box2::new(size * 0.13, size * 0.12, size * 0.87, size * 0.88);
            Self {
                size,
                tile_radius: size * 0.235,
                frame: None,
                sun: (
                    scene.left + scene.width() * 0.30,
                    scene.top + scene.height() * 0.31,
                ),
                sun_radius: scene.width() * 0.145,
                scene,
                scene_radius: 0.0,
                back_ridge: false,
                caption: None,
            }
        }
    }

    /// Color of one supersampled point, or `None` outside the rounded tile.
    fn shade(&self, x: f32, y: f32) -> Option<[f32; 3]> {
        let tile = Box2::new(0.0, 0.0, self.size, self.size);
        if !tile.rounded_contains(x, y, self.tile_radius) {
            return None;
        }
        let mut color = mix(TILE_TOP, TILE_BOTTOM, y / self.size);
        if let Some((frame, radius)) = self.frame {
            if frame.rounded_contains(x, y, radius) {
                color = WHITE;
            }
        }
        if self.scene.rounded_contains(x, y, self.scene_radius) {
            if self.frame.is_some() {
                let t = (y - self.scene.top) / self.scene.height();
                color = mix(PHOTO_TOP, PHOTO_BOTTOM, t);
            }
            let dx = x - self.sun.0;
            let dy = y - self.sun.1;
            if dx * dx + dy * dy <= self.sun_radius * self.sun_radius {
                color = mix(color, WHITE, 0.96);
            }
            let (w, h) = (self.scene.width(), self.scene.height());
            if self.back_ridge {
                let back = Triangle::new(
                    (self.scene.left, self.scene.bottom),
                    (self.scene.left + w * 0.34, self.scene.top + h * 0.48),
                    (self.scene.left + w * 0.66, self.scene.bottom),
                );
                if back.contains(x, y) {
                    color = mix(color, WHITE, 0.40);
                }
            }
            let front = Triangle::new(
                (self.scene.left + w * 0.22, self.scene.bottom),
                (self.scene.left + w * 0.55, self.scene.top + h * 0.58),
                (self.scene.right, self.scene.bottom),
            );
            if front.contains(x, y) {
                color = mix(color, WHITE, 0.95);
            }
        }
        if let Some((bar, radius)) = self.caption {
            if bar.rounded_contains(x, y, radius) {
                color = mix(color, TILE_BOTTOM, 0.24);
            }
        }
        Some(color)
    }
}

#[derive(Clone, Copy)]
struct Triangle {
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
}

impl Triangle {
    fn new(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> Self {
        Self { a, b, c }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        let sign = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
            (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1)
        };
        let point = (x, y);
        let d1 = sign(point, self.a, self.b);
        let d2 = sign(point, self.b, self.c);
        let d3 = sign(point, self.c, self.a);
        let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(negative && positive)
    }
}

#[derive(Clone, Copy)]
struct Box2 {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Box2 {
    fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    fn width(&self) -> f32 {
        self.right - self.left
    }

    fn height(&self) -> f32 {
        self.bottom - self.top
    }

    fn rounded_contains(&self, x: f32, y: f32, radius: f32) -> bool {
        if x < self.left || x > self.right || y < self.top || y > self.bottom {
            return false;
        }
        let radius = radius
            .max(0.0)
            .min(self.width() * 0.5)
            .min(self.height() * 0.5);
        if radius <= 0.0 {
            return true;
        }
        let cx = x.clamp(self.left + radius, self.right - radius);
        let cy = y.clamp(self.top + radius, self.bottom - radius);
        (x - cx) * (x - cx) + (y - cy) * (y - cy) <= radius * radius
    }
}

fn mix(from: [f32; 3], to: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        from[0] + (to[0] - from[0]) * t,
        from[1] + (to[1] - from[1]) * t,
        from[2] + (to[2] - from[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_renders_transparent_corners_and_an_opaque_mark() {
        for size in SIZES {
            let pixels = render(size);
            assert_eq!(pixels.len(), (size * size * 4) as usize, "size {size}");
            assert_eq!(pixels[3], 0, "top-left corner must be transparent");

            let center = ((size / 2) * size + size / 2) as usize * 4;
            assert_eq!(pixels[center + 3], 255, "center must be opaque");
        }
    }

    #[test]
    fn marks_are_deterministic_and_branded_teal() {
        let first = render(32);
        assert_eq!(first, render(32));

        // Just under the top edge, above the white card, the tile gradient shows.
        let y = 32 / 32;
        let index = (y * 32 + 16) * 4;
        let (r, g, b) = (first[index], first[index + 1], first[index + 2]);
        assert!(
            g > r + 40 && g > b,
            "the tile must read as teal: {r},{g},{b}"
        );
    }
}
