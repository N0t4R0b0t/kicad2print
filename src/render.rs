// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0

//! Software z-buffer rasterizer — renders a Mesh3D to a PNG image.

use crate::geometry::Mesh3D;
use image::{ImageBuffer, Rgb};

pub fn render_to_png(mesh: &Mesh3D, width: u32, height: u32) -> Vec<u8> {
    if mesh.triangles.is_empty() {
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::new(width, height);
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap_or_default();
        return out;
    }

    // Compute mesh bounding box
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for tri in &mesh.triangles {
        for v in &tri.vertices {
            for i in 0..3 {
                min[i] = min[i].min(v[i]);
                max[i] = max[i].max(v[i]);
            }
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];

    // Isometric-ish view: camera from upper-right-front
    // Build orthonormal basis: eye_dir, right, up
    let eye_dir = normalize([0.6, 0.5, 0.8]);
    let world_up = [0.0f32, 0.0, 1.0];
    let right = normalize(cross(eye_dir, world_up));
    let cam_up = normalize(cross(right, eye_dir));

    // Project a 3D point to (screen_x, screen_y, depth)
    let project = |p: [f32; 3]| -> (f32, f32, f32) {
        let rel = sub(p, center);
        let x = dot(rel, right);
        let y = dot(rel, cam_up);
        let z = dot(rel, eye_dir);
        (x, y, z)
    };

    // Find projected bounding box to fit view
    let mut px_min = f32::INFINITY;
    let mut px_max = f32::NEG_INFINITY;
    let mut py_min = f32::INFINITY;
    let mut py_max = f32::NEG_INFINITY;
    for tri in &mesh.triangles {
        for v in &tri.vertices {
            let (px, py, _) = project(*v);
            px_min = px_min.min(px);
            px_max = px_max.max(px);
            py_min = py_min.min(py);
            py_max = py_max.max(py);
        }
    }

    let padding = 0.05;
    let rx = px_max - px_min;
    let ry = py_max - py_min;
    let scale_x = (width as f32 * (1.0 - 2.0 * padding)) / rx.max(1e-6);
    let scale_y = (height as f32 * (1.0 - 2.0 * padding)) / ry.max(1e-6);
    let scale = scale_x.min(scale_y);

    let to_screen = |px: f32, py: f32| -> (i32, i32) {
        let sx = ((px - px_min) * scale + width as f32 * padding) as i32;
        // Flip Y so +Z is up on screen
        let sy = (height as f32 - ((py - py_min) * scale + height as f32 * padding)) as i32;
        (sx, sy)
    };

    let w = width as usize;
    let h = height as usize;
    let mut pixels = vec![[20u8, 20, 20]; w * h];
    let mut zbuf = vec![f32::NEG_INFINITY; w * h];

    // Light direction (same as eye, plus a bit from above)
    let light_dir = normalize([0.4, 0.3, 0.9]);

    for tri in &mesh.triangles {
        let (p0x, p0y, p0z) = project(tri.vertices[0]);
        let (p1x, p1y, p1z) = project(tri.vertices[1]);
        let (p2x, p2y, p2z) = project(tri.vertices[2]);

        let (s0x, s0y) = to_screen(p0x, p0y);
        let (s1x, s1y) = to_screen(p1x, p1y);
        let (s2x, s2y) = to_screen(p2x, p2y);

        // Flat shading: diffuse + ambient
        let ndotl = dot(tri.normal, light_dir).max(0.0);
        let brightness = 0.2 + 0.8 * ndotl;
        // Gold color: (212, 175, 55)
        let r = (212.0 * brightness).min(255.0) as u8;
        let g = (175.0 * brightness).min(255.0) as u8;
        let b = (55.0 * brightness).min(255.0) as u8;
        let color = [r, g, b];

        // Rasterize triangle using bounding box + barycentric test
        let bx0 = s0x.min(s1x).min(s2x).max(0);
        let bx1 = s0x.max(s1x).max(s2x).min(w as i32 - 1);
        let by0 = s0y.min(s1y).min(s2y).max(0);
        let by1 = s0y.max(s1y).max(s2y).min(h as i32 - 1);

        let denom = (s1y - s2y) * (s0x - s2x) + (s2x - s1x) * (s0y - s2y);
        if denom == 0 {
            continue;
        }
        let denom_f = denom as f32;

        for py in by0..=by1 {
            for px in bx0..=bx1 {
                let w0 = ((s1y - s2y) * (px - s2x) + (s2x - s1x) * (py - s2y)) as f32 / denom_f;
                let w1 = ((s2y - s0y) * (px - s2x) + (s0x - s2x) * (py - s2y)) as f32 / denom_f;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let depth = w0 * p0z + w1 * p1z + w2 * p2z;
                let idx = py as usize * w + px as usize;
                if depth > zbuf[idx] {
                    zbuf[idx] = depth;
                    pixels[idx] = color;
                }
            }
        }
    }

    let img: ImageBuffer<Rgb<u8>, _> =
        ImageBuffer::from_fn(width, height, |x, y| {
            let c = pixels[y as usize * w + x as usize];
            Rgb(c)
        });

    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap_or_default();
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}
