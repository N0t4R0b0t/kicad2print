// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Core PCB data types representing parsed KiCad design elements.
//!
//! This module defines the fundamental data structures that represent a KiCad PCB design.
//! These types are populated by the parser and consumed by the geometry generation pipeline.
//!
//! The coordinate system used here has already been corrected from KiCad's Y-down convention
//! to standard mathematical convention (Y-up), so all `Point2` coordinates are in standard form.

/// A 2D point in millimeters.
///
/// Coordinates are already in standard mathematical convention (X right, Y up).
/// KiCad's Y-axis inversion is applied during parsing, so values here represent
/// the final corrected coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    /// Creates a new 2D point.
    pub fn new(x: f64, y: f64) -> Self {
        Point2 { x, y }
    }

    /// Computes the Euclidean distance to another point.
    pub fn distance_to(&self, other: Point2) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }

    /// Scales this point by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        Point2 {
            x: self.x * factor,
            y: self.y * factor,
        }
    }

    /// Translates this point by an offset.
    #[allow(dead_code)]
    pub fn translate(&self, dx: f64, dy: f64) -> Self {
        Point2 {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

/// Represents which copper layer a trace is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopperLayer {
    /// Front copper layer (top face when looking at the board)
    FCu,
    /// Back copper layer (bottom face when looking at the board)
    BCu,
}

/// A straight trace segment on a copper layer.
///
/// Traces are the electrical connections on the PCB. In the hybrid 3D-printed
/// construction method, traces are represented as grooves/channels routed into
/// the printed substrate, and then wires are laid into these channels.
#[derive(Debug, Clone)]
pub struct Trace {
    /// Which copper layer this trace is on
    pub layer: CopperLayer,
    /// Starting point of the trace segment
    pub start: Point2,
    /// Ending point of the trace segment
    pub end: Point2,
    /// Width of the trace in millimeters
    ///
    /// Note: this is informational and comes from KiCad, but the actual channel
    /// width is determined by the configuration's `channel_width_mm` parameter.
    pub width: f64,
}

impl Trace {
    /// Computes the length of this trace segment.
    #[allow(dead_code)]
    pub fn length(&self) -> f64 {
        self.start.distance_to(self.end)
    }

    /// Scales this trace by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        Trace {
            layer: self.layer,
            start: self.start.scale(factor),
            end: self.end.scale(factor),
            width: self.width * factor,
        }
    }
}

/// An arc trace segment on a copper layer.
///
/// KiCad 7+ uses the three-point arc format (start, midpoint, end) to define arcs
/// unambiguously. Arcs are less common than straight traces in typical designs.
#[derive(Debug, Clone)]
pub struct ArcTrace {
    /// Which copper layer this arc is on
    pub layer: CopperLayer,
    /// Starting point of the arc
    pub start: Point2,
    /// A point on the arc (used to disambiguate which arc to use)
    pub mid: Point2,
    /// Ending point of the arc
    pub end: Point2,
}

impl ArcTrace {
    /// Scales this arc by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        ArcTrace {
            layer: self.layer,
            start: self.start.scale(factor),
            mid: self.mid.scale(factor),
            end: self.end.scale(factor),
        }
    }
}

/// A via (vertical interconnect access) connecting front and back copper layers.
///
/// Vias are holes that allow electrical connections between the top and bottom
/// of the board. In the hybrid construction method, vias are either:
/// - Full through-holes (eyelet_style = "hole"): drilled with copper eyelets
/// - Shallow indents (eyelet_style = "indent"): small dimples that guide solder
#[derive(Debug, Clone, Copy)]
pub struct Via {
    /// Center point of the via hole
    pub center: Point2,
    /// Diameter of the via hole (drill size) in millimeters
    pub drill: f64,
}

impl Via {
    /// Scales this via by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        Via {
            center: self.center.scale(factor),
            drill: self.drill * factor,
        }
    }
}

/// A component pad that has a through-hole.
///
/// Pads are the connection points for component leads (resistors, capacitors, etc.).
/// This type only tracks through-hole pads; SMD (surface-mount) pads without holes
/// are ignored since they don't require substrate modifications.
#[derive(Debug, Clone, Copy)]
pub struct Pad {
    /// Center point of the pad
    pub center: Point2,
    /// Drill hole diameter in millimeters
    pub drill: f64,
}

impl Pad {
    /// Scales this pad by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        Pad {
            center: self.center.scale(factor),
            drill: self.drill * factor,
        }
    }
}

/// A placed component footprint with its reference designator, value, and pad locations.
#[derive(Debug, Clone)]
pub struct Footprint {
    /// Reference designator (e.g. "R1", "C3", "U2")
    pub reference: String,
    /// Component value (e.g. "10k", "100nF", "ATmega328P")
    pub value: String,
    /// Center position of the footprint on the board
    pub position: Point2,
    /// Through-hole pads belonging to this footprint
    pub pads: Vec<Pad>,
}

/// Axis-aligned bounding box for the board.
///
/// Used to determine substrate dimensions and for coordinate transformations.
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
    /// Minimum X coordinate
    pub min_x: f64,
    /// Minimum Y coordinate
    pub min_y: f64,
    /// Maximum X coordinate
    pub max_x: f64,
    /// Maximum Y coordinate
    pub max_y: f64,
}

impl BoundingBox {
    /// Creates a new bounding box.
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        BoundingBox {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    /// Computes the width of the bounding box.
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    /// Computes the height of the bounding box.
    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    /// Scales this bounding box by a factor.
    #[allow(dead_code)]
    pub fn scale(&self, factor: f64) -> Self {
        BoundingBox {
            min_x: self.min_x * factor,
            min_y: self.min_y * factor,
            max_x: self.max_x * factor,
            max_y: self.max_y * factor,
        }
    }

    /// Updates the bounding box to include a point.
    pub fn expand_to_include(&mut self, point: Point2) {
        self.min_x = self.min_x.min(point.x);
        self.min_y = self.min_y.min(point.y);
        self.max_x = self.max_x.max(point.x);
        self.max_y = self.max_y.max(point.y);
    }
}

/// The physical outline of the PCB.
///
/// The outline is extracted from the `Edge.Cuts` layer in KiCad and defines
/// the perimeter of the board that will be 3D-printed. It can be a simple
/// rectangle or an arbitrary polygon.
#[derive(Debug, Clone)]
pub struct BoardOutline {
    /// Vertices of the outline polygon (form a closed loop)
    /// The last vertex connects back to the first to close the shape.
    pub vertices: Vec<Point2>,
    /// Bounding box enclosing all vertices
    pub bbox: BoundingBox,
}

impl BoardOutline {
    /// Creates a new board outline from vertices.
    ///
    /// Automatically computes the bounding box from the provided vertices.
    pub fn new(vertices: Vec<Point2>) -> Self {
        let mut bbox = BoundingBox::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &vertex in &vertices {
            bbox.expand_to_include(vertex);
        }
        BoardOutline { vertices, bbox }
    }

    /// Computes the perimeter of the outline.
    pub fn perimeter(&self) -> f64 {
        let mut total = 0.0;
        for i in 0..self.vertices.len() {
            let current = self.vertices[i];
            let next = self.vertices[(i + 1) % self.vertices.len()];
            total += current.distance_to(next);
        }
        total
    }

    /// Scales this outline by a factor (for board scaling).
    pub fn scale(&self, factor: f64) -> Self {
        let vertices = self.vertices.iter().map(|v| v.scale(factor)).collect();
        BoardOutline::new(vertices)
    }
}

/// A cutout shape in the board (from Edge.Cuts graphics inside footprints or top-level).
#[derive(Debug, Clone, Copy)]
pub enum CutoutShape {
    /// Rectangular cutout: (center_x, center_y, half_width, half_height, rotation_deg)
    Rect { cx: f64, cy: f64, hw: f64, hh: f64, rot: f64 },
    /// Circular cutout: (center_x, center_y, radius)
    Circle { cx: f64, cy: f64, r: f64 },
}

/// Complete parsed PCB design data.
///
/// This struct holds all the extracted information from a KiCad `.kicad_pcb` file.
/// It serves as the interface between the parser and the geometry generation pipeline.
///
/// # Example
/// ```no_run
/// let pcb_data = parse_pcb("board.kicad_pcb")?;
/// println!("Found {} traces on F.Cu", pcb_data.traces_fcu.len());
/// println!("Found {} vias", pcb_data.vias.len());
/// ```
#[derive(Debug, Default)]
pub struct PcbData {
    /// Physical outline of the PCB (from Edge.Cuts layer)
    /// This is required for determining board dimensions.
    pub outline: Option<BoardOutline>,

    /// Straight trace segments on the front copper layer
    pub traces_fcu: Vec<Trace>,

    /// Straight trace segments on the back copper layer
    pub traces_bcu: Vec<Trace>,

    /// Arc-shaped trace segments (less common)
    pub arc_traces: Vec<ArcTrace>,

    /// Via connections between front and back layers
    pub vias: Vec<Via>,

    /// Component pad through-holes
    pub pads: Vec<Pad>,

    /// Parsed footprints with reference designators, values, and positions
    pub footprints: Vec<Footprint>,

    /// Board cutouts from Edge.Cuts graphics (fp_rect, gr_rect, gr_circle, etc.)
    pub cutouts: Vec<CutoutShape>,
}

impl PcbData {
    /// Scales all geometry in this PCB data by a uniform factor.
    ///
    /// This is used to ensure that trace channels can accommodate the configured
    /// minimum width, or to adjust board size as needed.
    pub fn scale(&self, factor: f64) -> Self {
        PcbData {
            outline: self.outline.as_ref().map(|o| o.scale(factor)),
            traces_fcu: self.traces_fcu.iter().map(|t| t.scale(factor)).collect(),
            traces_bcu: self.traces_bcu.iter().map(|t| t.scale(factor)).collect(),
            arc_traces: self.arc_traces.iter().map(|a| a.scale(factor)).collect(),
            vias: self.vias.iter().map(|v| v.scale(factor)).collect(),
            pads: self.pads.iter().map(|p| p.scale(factor)).collect(),
            footprints: self.footprints.iter().map(|f| Footprint {
                reference: f.reference.clone(),
                value: f.value.clone(),
                position: f.position.scale(factor),
                pads: f.pads.iter().map(|p| p.scale(factor)).collect(),
            }).collect(),
            cutouts: self.cutouts.iter().map(|c| match *c {
                CutoutShape::Rect { cx, cy, hw, hh, rot } => CutoutShape::Rect { cx: cx * factor, cy: cy * factor, hw: hw * factor, hh: hh * factor, rot },
                CutoutShape::Circle { cx, cy, r } => CutoutShape::Circle { cx: cx * factor, cy: cy * factor, r: r * factor },
            }).collect(),
        }
    }

    /// Computes total number of design elements.
    pub fn element_count(&self) -> usize {
        self.traces_fcu.len()
            + self.traces_bcu.len()
            + self.arc_traces.len()
            + self.vias.len()
            + self.pads.len()
    }

    /// Prints a summary of the parsed design to stdout.
    ///
    /// Useful for debugging and verification.
    pub fn print_summary(&self) {
        println!("=== PCB Design Summary ===");
        if let Some(outline) = &self.outline {
            println!("Board size: {:.2}mm × {:.2}mm", outline.bbox.width(), outline.bbox.height());
            println!("Board perimeter: {:.2}mm", outline.perimeter());
        } else {
            println!("WARNING: No board outline found!");
        }
        println!("Front copper traces: {}", self.traces_fcu.len());
        println!("Back copper traces:  {}", self.traces_bcu.len());
        println!("Arc traces:          {}", self.arc_traces.len());
        println!("Vias:                {}", self.vias.len());
        println!("Pads with drills:    {}", self.pads.len());
        println!("Total elements:      {}", self.element_count());
    }
}
