// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! HTML 3D preview generator using three.js

use crate::geometry::Mesh3D;
use anyhow::Result;
use std::io::Write;
use std::path::Path;

pub fn write(mesh: &Mesh3D, stem: &str, path: &Path) -> Result<()> {
    let mut file = std::fs::File::create(path)?;

    let vertices_json = generate_vertices_json(mesh);
    let indices_json = generate_indices_json(mesh);

    // Compute bounding box for stats
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
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];

    let html = format_html(stem, mesh.triangle_count(), size[0], size[1], size[2], &vertices_json, &indices_json);

    file.write_all(html.as_bytes())?;
    Ok(())
}

fn format_html(stem: &str, tri_count: usize, sx: f32, sy: f32, sz: f32, vertices: &str, indices: &str) -> String {
    let js_vertices = vertices;
    let js_indices = indices;

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>kicad2print 3D Preview - {}</title>
    <style>
        body {{
            margin: 0;
            overflow: hidden;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
            background: #1a1a1a;
            color: #fff;
        }}
        #info {{
            position: absolute;
            top: 10px;
            left: 10px;
            background: rgba(0, 0, 0, 0.7);
            padding: 15px;
            border-radius: 8px;
            font-size: 14px;
            line-height: 1.6;
            max-width: 300px;
            z-index: 10;
        }}
        #info h2 {{
            margin: 0 0 10px 0;
            font-size: 16px;
        }}
        #info p {{
            margin: 5px 0;
        }}
        #controls {{
            position: absolute;
            bottom: 10px;
            left: 10px;
            background: rgba(0, 0, 0, 0.7);
            padding: 15px;
            border-radius: 8px;
            font-size: 12px;
            z-index: 10;
        }}
        #controls p {{
            margin: 5px 0;
        }}
    </style>
</head>
<body>
    <div id="info">
        <h2>kicad2print Preview</h2>
        <p><strong>{}</strong></p>
        <p>Triangles: {}</p>
        <p>Size: {:.1} × {:.1} × {:.1} mm</p>
    </div>
    <div id="controls">
        <p><strong>Controls:</strong></p>
        <p>🖱️ Drag: Rotate</p>
        <p>🖱️ Scroll: Zoom</p>
        <p>Right-Click Drag: Pan</p>
    </div>

    <script src="https://cdnjs.cloudflare.com/ajax/libs/three.js/r128/three.min.js"></script>
    <script>
        const scene = new THREE.Scene();
        scene.background = new THREE.Color(0x2a2a2a);
        const camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.1, 10000);
        const renderer = new THREE.WebGLRenderer({{ antialias: true }});
        renderer.setSize(window.innerWidth, window.innerHeight);
        renderer.shadowMap.enabled = true;
        document.body.appendChild(renderer.domElement);

        const ambientLight = new THREE.AmbientLight(0xffffff, 0.6);
        scene.add(ambientLight);
        const directionalLight = new THREE.DirectionalLight(0xffffff, 0.8);
        directionalLight.position.set(10, 20, 10);
        directionalLight.castShadow = true;
        scene.add(directionalLight);

        const vertices = {js_vertices};
        const indices = {js_indices};
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', new THREE.BufferAttribute(new Float32Array(vertices), 3));
        geometry.setIndex(new THREE.BufferAttribute(new Uint32Array(indices), 1));
        geometry.computeVertexNormals();

        const material = new THREE.MeshPhongMaterial({{ color: 0xffd700, shininess: 100 }});
        const mesh = new THREE.Mesh(geometry, material);
        mesh.castShadow = true;
        mesh.receiveShadow = true;
        scene.add(mesh);

        const gridHelper = new THREE.GridHelper(200, 20, 0x444444, 0x222222);
        scene.add(gridHelper);

        geometry.computeBoundingBox();
        const box = geometry.boundingBox;
        const center = new THREE.Vector3();
        box.getCenter(center);
        const size = box.getSize(new THREE.Vector3());
        const maxDim = Math.max(size.x, size.y, size.z);
        const fov = camera.fov * (Math.PI / 180);
        let cameraZ = Math.abs(maxDim / 2 / Math.tan(fov / 2)) * 1.5;
        camera.position.set(center.x, center.y + size.z / 2, center.z + cameraZ);
        camera.lookAt(center);

        let isDragging = false, isPanning = false;
        let prevMouse = {{ x: 0, y: 0 }};
        const rotation = {{ x: 0, y: 0 }};

        renderer.domElement.addEventListener('mousedown', (e) => {{
            isDragging = e.button === 0;
            isPanning = e.button === 2;
            prevMouse = {{ x: e.clientX, y: e.clientY }};
        }});

        renderer.domElement.addEventListener('mousemove', (e) => {{
            if (isDragging) {{
                rotation.y += (e.clientX - prevMouse.x) * 0.01;
                rotation.x += (e.clientY - prevMouse.y) * 0.01;
                mesh.rotation.y = rotation.y;
                mesh.rotation.x = rotation.x;
            }} else if (isPanning) {{
                camera.position.x -= (e.clientX - prevMouse.x) * 0.1;
                camera.position.y += (e.clientY - prevMouse.y) * 0.1;
            }}
            prevMouse = {{ x: e.clientX, y: e.clientY }};
        }});

        renderer.domElement.addEventListener('mouseup', () => {{
            isDragging = false;
            isPanning = false;
        }});

        renderer.domElement.addEventListener('wheel', (e) => {{
            e.preventDefault();
            const dir = camera.position.clone().sub(center).normalize();
            const dist = camera.position.distanceTo(center);
            const newDist = e.deltaY > 0 ? dist * 1.1 : dist / 1.1;
            camera.position.copy(center.clone().add(dir.multiplyScalar(newDist)));
        }});

        renderer.domElement.addEventListener('contextmenu', e => e.preventDefault());

        function animate() {{
            requestAnimationFrame(animate);
            renderer.render(scene, camera);
        }}
        animate();

        window.addEventListener('resize', () => {{
            camera.aspect = window.innerWidth / window.innerHeight;
            camera.updateProjectionMatrix();
            renderer.setSize(window.innerWidth, window.innerHeight);
        }});
    </script>
</body>
</html>"#,
        stem, stem, tri_count, sx, sy, sz
    )
}

fn generate_vertices_json(mesh: &Mesh3D) -> String {
    let mut verts = Vec::new();
    for tri in &mesh.triangles {
        for vertex in &tri.vertices {
            verts.push(vertex[0]);
            verts.push(vertex[1]);
            verts.push(vertex[2]);
        }
    }

    let json = verts
        .iter()
        .map(|v| format!("{:.4}", v))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{}]", json)
}

fn generate_indices_json(mesh: &Mesh3D) -> String {
    let indices: Vec<u32> = (0..mesh.triangles.len() as u32 * 3).collect();

    let json = indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("[{}]", json)
}
