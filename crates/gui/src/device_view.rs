//! The Device tab: the die as ISE placed and routed the design.
//!
//! Tiles are drawn on a grid (row 0 at the top, as in the XDLRC), CLB tiles
//! with their four slices, placed instances filled by module colour. On top:
//! a routing-density heat map, a selected instance or net (with flylines from
//! driver to loads), and a critical timing path step by step. Pan by
//! dragging, zoom with Ctrl + wheel (or pinch).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use fpgaflow_flow::layout_model::{site_xy, Device, Placed, TileClass};
use fpgaflow_flow::LayoutData;

/// Scene units per tile.
const S: f32 = 12.0;

/// Module colours (a qualitative palette that reads on dark and light).
const PALETTE: [Color32; 12] = [
    Color32::from_rgb(78, 121, 167),
    Color32::from_rgb(242, 142, 43),
    Color32::from_rgb(225, 87, 89),
    Color32::from_rgb(118, 183, 178),
    Color32::from_rgb(89, 161, 79),
    Color32::from_rgb(237, 201, 72),
    Color32::from_rgb(176, 122, 161),
    Color32::from_rgb(255, 157, 167),
    Color32::from_rgb(156, 117, 95),
    Color32::from_rgb(140, 200, 240),
    Color32::from_rgb(190, 220, 90),
    Color32::from_rgb(200, 140, 240),
];

/// Pre-computed drawing geometry for one loaded layout.
struct Geometry {
    extent: Rect,
    tiles: Vec<(Rect, TileClass)>,
    /// Site name -> rectangle.
    sites: HashMap<String, Rect>,
    /// (row, col) -> tile index, for hit-testing.
    grid: HashMap<(u32, u32), usize>,
    /// Tile name -> rectangle (routing PIPs are reported per tile).
    tile_by_name: HashMap<String, Rect>,
    /// Module -> colour.
    colours: BTreeMap<String, Color32>,
    /// Instance index -> rectangle (placed instances only).
    inst_rects: Vec<Option<Rect>>,
    /// Instance name -> nets it drives.
    drives: HashMap<String, Vec<usize>>,
    /// Tile name -> routing PIPs, and the maximum.
    density: HashMap<String, u32>,
    max_density: u32,
}

fn site_rects(d: &Device) -> HashMap<String, Rect> {
    let mut out = HashMap::new();
    for t in &d.tiles {
        let tile = Rect::from_min_size(
            Pos2::new(t.col as f32 * S, t.row as f32 * S),
            Vec2::splat(S),
        );
        let n = t.sites.len();
        for (k, &si) in t.sites.iter().enumerate() {
            let s = &d.sites[si];
            let r = if s.kind.starts_with("SLICE") {
                // 2 x 2: SLICEMs (even X) on the left, odd Y on top.
                let (x, y) = site_xy(&s.name).unwrap_or((0, 0));
                let half = S / 2.0;
                let min = tile.min + Vec2::new((x % 2) as f32 * half, (1 - y % 2) as f32 * half);
                Rect::from_min_size(min, Vec2::splat(half)).shrink(0.6)
            } else if n > 1 {
                // Several sites (IOBs, BRAM + MULT): stack them.
                let h = S / n as f32;
                Rect::from_min_size(tile.min + Vec2::new(0.0, k as f32 * h), Vec2::new(S, h))
                    .shrink(0.5)
            } else {
                tile.shrink(0.8)
            };
            out.insert(s.name.clone(), r);
        }
    }
    out
}

fn colours(placed: Option<&Placed>) -> BTreeMap<String, Color32> {
    let mut c = BTreeMap::new();
    let Some(p) = placed else { return c };
    let mut i = 0;
    for m in p.by_module().keys() {
        let col = match m.as_str() {
            "<top>" => Color32::from_gray(170),
            "<io>" => Color32::from_rgb(205, 170, 110),
            "<ise>" => Color32::from_gray(110),
            _ => {
                let col = PALETTE[i % PALETTE.len()];
                i += 1;
                col
            }
        };
        c.insert(m.clone(), col);
    }
    c
}

impl Geometry {
    fn new(l: &LayoutData) -> Geometry {
        let d = &l.device;
        let sites = site_rects(d);
        let tiles = d
            .tiles
            .iter()
            .map(|t| {
                (
                    Rect::from_min_size(
                        Pos2::new(t.col as f32 * S, t.row as f32 * S),
                        Vec2::splat(S),
                    ),
                    t.class,
                )
            })
            .collect();
        let grid = d
            .tiles
            .iter()
            .enumerate()
            .map(|(i, t)| ((t.row, t.col), i))
            .collect();
        let tile_by_name = d
            .tiles
            .iter()
            .zip(&tiles)
            .map(|(t, (r, _)): (_, &(Rect, TileClass))| (t.name.clone(), *r))
            .collect();
        let placed = l.placed.as_ref();
        let inst_rects = placed
            .map(|p| {
                p.insts
                    .iter()
                    .map(|i| sites.get(&i.site).copied())
                    .collect()
            })
            .unwrap_or_default();
        let mut drives: HashMap<String, Vec<usize>> = HashMap::new();
        let mut density = HashMap::new();
        if let Some(p) = placed {
            for (k, n) in p.nets.iter().enumerate() {
                if let Some(dr) = n.driver() {
                    drives.entry(dr.inst.clone()).or_default().push(k);
                }
            }
            density = p
                .pip_density()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
        }
        let max_density = density.values().copied().max().unwrap_or(1).max(1);
        Geometry {
            extent: Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(d.cols as f32 * S, d.rows as f32 * S),
            ),
            tiles,
            sites,
            grid,
            tile_by_name,
            colours: colours(placed),
            inst_rects,
            drives,
            density,
            max_density,
        }
    }
}

pub struct DeviceView {
    pub data: Option<Arc<LayoutData>>,
    pub loading: bool,
    pub error: Option<String>,
    /// A newer build exists than the data shown.
    pub stale: bool,
    geom: Option<Geometry>,
    scene_rect: Rect,
    show_heat: bool,
    hidden: BTreeSet<String>,
    search: String,
    selected_inst: Option<usize>,
    selected_net: Option<usize>,
    selected_path: Option<usize>,
    hover: Option<String>,
}

impl Default for DeviceView {
    fn default() -> Self {
        DeviceView {
            data: None,
            loading: false,
            error: None,
            stale: false,
            geom: None,
            scene_rect: Rect::ZERO,
            show_heat: false,
            hidden: BTreeSet::new(),
            search: String::new(),
            selected_inst: None,
            selected_net: None,
            selected_path: None,
            hover: None,
        }
    }
}

/// What the tab wants the app to do.
pub enum DeviceRequest {
    Load,
}

impl DeviceView {
    pub fn set_data(&mut self, data: Result<Arc<LayoutData>, String>) {
        self.loading = false;
        match data {
            Ok(d) => {
                self.geom = Some(Geometry::new(&d));
                self.data = Some(d);
                self.error = None;
                self.stale = false;
                self.selected_inst = None;
                self.selected_net = None;
                self.selected_path = None;
                self.scene_rect = Rect::ZERO; // refit
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn clear(&mut self) {
        *self = DeviceView::default();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<DeviceRequest> {
        let mut req = None;
        if self.data.is_none() && !self.loading && self.error.is_none() {
            req = Some(DeviceRequest::Load);
        }
        let top_down = egui::Layout::top_down(egui::Align::LEFT);
        ui.horizontal_top(|ui| {
            let h = ui.available_height();
            ui.allocate_ui_with_layout(Vec2::new(280.0, h), top_down, |ui| {
                ui.set_width(280.0);
                ui.set_min_height(h);
                egui::ScrollArea::vertical()
                    .id_salt("device_side")
                    .show(ui, |ui| {
                        if let Some(r) = self.side(ui) {
                            req = Some(r);
                        }
                    });
            });
            ui.separator();
            let rest = ui.available_size();
            ui.allocate_ui_with_layout(rest, top_down, |ui| self.scene(ui));
        });
        req
    }

    fn side(&mut self, ui: &mut egui::Ui) -> Option<DeviceRequest> {
        let mut req = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.loading, egui::Button::new("Reload"))
                .on_hover_text("Re-read the device and the last build")
                .clicked()
            {
                req = Some(DeviceRequest::Load);
            }
            if ui.button("Fit").clicked() {
                self.scene_rect = Rect::ZERO;
            }
            if self.loading {
                ui.spinner();
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(220, 80, 70), e);
        }
        if self.stale {
            ui.colored_label(
                Color32::from_rgb(200, 170, 60),
                "a newer build exists: Reload",
            );
        }
        let Some(l) = self.data.clone() else {
            if !self.loading && self.error.is_none() {
                ui.label("Loading…");
            }
            return req;
        };
        let d = &l.device;
        ui.label(RichText::new(&l.part).strong());
        ui.small(format!(
            "{} slices, {} pads, {} MULT18X18, {} RAMB16",
            d.count("SLICE"),
            d.bonded_pads(),
            d.count("MULT18X18"),
            d.count("RAMB16")
        ));
        let Some(p) = l.placed.as_ref() else {
            ui.separator();
            ui.label("No placed design yet: run Build.");
            return req;
        };
        ui.separator();

        // Search.
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("find instance or net")
                    .desired_width(190.0),
            );
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Find").clicked()
            {
                self.find(p);
            }
        });
        ui.checkbox(&mut self.show_heat, "Routing density");

        // Legend.
        ui.separator();
        ui.label(RichText::new("Modules").strong());
        let geom = self.geom.as_ref();
        egui::Grid::new("legend").num_columns(3).show(ui, |ui| {
            for (m, u) in p.by_module() {
                let col = geom
                    .and_then(|g| g.colours.get(&m).copied())
                    .unwrap_or(Color32::GRAY);
                let mut shown = !self.hidden.contains(&m);
                if ui.checkbox(&mut shown, "").changed() {
                    if shown {
                        self.hidden.remove(&m);
                    } else {
                        self.hidden.insert(m.clone());
                    }
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, col);
                    ui.label(&m);
                });
                let mut what = vec![];
                if u.slices > 0 {
                    what.push(format!("{} sl", u.slices));
                }
                if u.pads > 0 {
                    what.push(format!("{} io", u.pads));
                }
                if u.mults > 0 {
                    what.push(format!("{} mult", u.mults));
                }
                if u.brams > 0 {
                    what.push(format!("{} bram", u.brams));
                }
                ui.small(what.join(", "));
                ui.end_row();
            }
        });

        // Selection.
        if let Some(i) = self.selected_inst {
            ui.separator();
            let inst = &p.insts[i];
            ui.label(RichText::new("Instance").strong());
            ui.monospace(&inst.name);
            ui.small(format!(
                "{} at {} ({}), module {}",
                inst.kind, inst.site, inst.tile, inst.module
            ));
            if let Some(nets) = geom.and_then(|g| g.drives.get(&inst.name)) {
                ui.small("drives:");
                for &k in nets.iter().take(20) {
                    if ui.link(&p.nets[k].name).clicked() {
                        self.selected_net = Some(k);
                    }
                }
            }
        }
        if let Some(k) = self.selected_net {
            ui.separator();
            let n = &p.nets[k];
            ui.label(RichText::new("Net").strong());
            ui.monospace(&n.name);
            ui.small(format!(
                "{} loads, {} PIPs in {} tiles",
                n.pins.iter().filter(|x| !x.output).count(),
                n.pip_count(),
                n.pip_tiles.len()
            ));
            if ui.small_button("clear").clicked() {
                self.selected_net = None;
            }
        }

        // Critical paths.
        ui.separator();
        ui.label(RichText::new("Critical paths").strong());
        if l.paths.is_empty() {
            ui.small("no timing report");
        }
        for (k, path) in l.paths.iter().take(10).enumerate() {
            let col = if path.slack_ns < 0.0 {
                Color32::from_rgb(220, 80, 70)
            } else {
                Color32::from_rgb(60, 170, 90)
            };
            let label = format!(
                "{:+.3} ns  {} levels",
                path.slack_ns,
                path.levels.map(|n| n.to_string()).unwrap_or_default()
            );
            let sel = self.selected_path == Some(k);
            if ui
                .selectable_label(sel, RichText::new(label).color(col))
                .on_hover_text(format!("{}\n→ {}", path.source, path.dest))
                .clicked()
            {
                self.selected_path = if sel { None } else { Some(k) };
            }
            if sel {
                for s in &path.steps {
                    ui.small(format!(
                        "{}.{}  {} {:.3}",
                        s.site, s.pin, s.kind, s.delay_ns
                    ));
                }
            }
        }
        req
    }

    fn find(&mut self, p: &Placed) {
        let q = self.search.trim();
        if q.is_empty() {
            return;
        }
        let hit = |name: &str| name == q || name.contains(q);
        if let Some(i) = p.insts.iter().position(|i| hit(&i.name)) {
            self.selected_inst = Some(i);
            self.selected_net = None;
            if let Some(r) = self.geom.as_ref().and_then(|g| g.inst_rects[i]) {
                self.scene_rect = Rect::from_center_size(r.center(), Vec2::splat(20.0 * S));
            }
        } else if let Some(k) = p.nets.iter().position(|n| hit(&n.name)) {
            self.selected_net = Some(k);
        }
    }

    fn scene(&mut self, ui: &mut egui::Ui) {
        let (Some(l), Some(g)) = (self.data.clone(), self.geom.as_ref()) else {
            return;
        };
        let dark = ui.visuals().dark_mode;
        let tile_col = |c: TileClass| -> Color32 {
            let (d, li) = match c {
                TileClass::Clb => ((48, 50, 56), (228, 230, 234)),
                TileClass::Bram => ((66, 48, 84), (226, 214, 238)),
                TileClass::Iob => ((80, 66, 44), (238, 226, 205)),
                TileClass::Dcm => ((38, 76, 76), (205, 232, 232)),
                TileClass::Clock => ((70, 70, 32), (236, 236, 200)),
                TileClass::Other => ((32, 33, 36), (245, 245, 247)),
            };
            let (r, gr, b) = if dark { d } else { li };
            Color32::from_rgb(r, gr, b)
        };
        let empty_site = if dark {
            Color32::from_gray(66)
        } else {
            Color32::from_gray(205)
        };
        // The site (or, between sites, the tile) under a scene position.
        let at = |pos: Pos2| -> Option<String> {
            let key = ((pos.y / S).floor() as u32, (pos.x / S).floor() as u32);
            let t = &l.device.tiles[*g.grid.get(&key)?];
            t.sites
                .iter()
                .map(|&s| &l.device.sites[s].name)
                .find(|n| g.sites.get(*n).is_some_and(|r| r.contains(pos)))
                .cloned()
                .or_else(|| Some(t.name.clone()))
        };
        let describe = |site: &str| -> String {
            l.placed
                .as_ref()
                .and_then(|p| p.insts.iter().find(|i| i.site == site))
                .map(|i| format!("{site}: {} ({}, {})", i.name, i.kind, i.module))
                .unwrap_or_else(|| site.to_string())
        };
        let mut rect = self.scene_rect;
        let mut clicked: Option<Pos2> = None;
        let mut hover_pos: Option<Pos2> = None;
        egui::Scene::new()
            .zoom_range(0.05..=24.0)
            .show(ui, &mut rect, |ui| {
                let resp = ui.allocate_rect(g.extent, Sense::click());
                hover_pos = resp.hover_pos();
                if resp.clicked() {
                    clicked = resp.hover_pos();
                }
                if let Some(site) = hover_pos.and_then(at) {
                    resp.on_hover_text_at_pointer(describe(&site));
                }
                let painter = ui.painter();
                for (r, c) in &g.tiles {
                    if *c != TileClass::Other {
                        painter.rect_filled(*r, 0.0, tile_col(*c));
                    }
                }
                for r in g.sites.values() {
                    painter.rect_filled(*r, 0.5, empty_site);
                }
                if self.show_heat {
                    for (name, &n) in &g.density {
                        if let Some(r) = g.tile_by_name.get(name) {
                            let a = (n as f32 / g.max_density as f32).sqrt();
                            painter.rect_filled(
                                *r,
                                0.0,
                                Color32::from_rgba_unmultiplied(255, 60, 20, (a * 200.0) as u8),
                            );
                        }
                    }
                }
                if let Some(p) = &l.placed {
                    for (i, inst) in p.insts.iter().enumerate() {
                        if self.hidden.contains(&inst.module) {
                            continue;
                        }
                        if let Some(r) = g.inst_rects[i] {
                            let c = g
                                .colours
                                .get(&inst.module)
                                .copied()
                                .unwrap_or(Color32::GRAY);
                            painter.rect_filled(r, 0.5, c);
                        }
                    }
                    self.draw_selection(painter, p, g);
                }
                if let Some(k) = self.selected_path {
                    if let Some(path) = l.paths.get(k) {
                        let mut pts: Vec<Pos2> = Vec::new();
                        for s in &path.steps {
                            if let Some(r) = g.sites.get(&s.site) {
                                if pts.last() != Some(&r.center()) {
                                    pts.push(r.center());
                                }
                                painter.rect_stroke(
                                    *r,
                                    0.5,
                                    Stroke::new(0.8, Color32::WHITE),
                                    egui::StrokeKind::Outside,
                                );
                            }
                        }
                        let col = if path.slack_ns < 0.0 {
                            Color32::from_rgb(255, 70, 60)
                        } else {
                            Color32::from_rgb(255, 230, 60)
                        };
                        painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.2, col)));
                        if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
                            painter.circle_filled(*a, 1.6, col);
                            painter.circle_stroke(*b, 2.0, Stroke::new(1.0, col));
                        }
                    }
                }
            });
        self.scene_rect = rect;

        self.hover = hover_pos.and_then(at);
        if let (Some(pos), Some(p)) = (clicked, &l.placed) {
            if let Some(site) = at(pos) {
                self.selected_inst = p.insts.iter().position(|i| i.site == site);
            }
        }
    }

    fn draw_selection(&self, painter: &egui::Painter, p: &Placed, g: &Geometry) {
        let hi = Color32::from_rgb(255, 255, 255);
        if let Some(i) = self.selected_inst {
            if let Some(r) = g.inst_rects[i] {
                painter.rect_stroke(
                    r.expand(1.0),
                    1.0,
                    Stroke::new(1.2, hi),
                    egui::StrokeKind::Outside,
                );
            }
        }
        if let Some(k) = self.selected_net {
            let n = &p.nets[k];
            for (t, _) in &n.pip_tiles {
                if let Some(&tile) = g.tile_by_name.get(t) {
                    painter.rect_stroke(
                        tile,
                        0.0,
                        Stroke::new(0.6, Color32::from_rgb(90, 200, 255)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            let centre = |inst: &str| {
                p.inst(inst)
                    .and_then(|x| g.sites.get(&x.site))
                    .map(|r| r.center())
            };
            if let Some(from) = n.driver().and_then(|d| centre(&d.inst)) {
                painter.circle_filled(from, 1.8, hi);
                for pin in n.pins.iter().filter(|x| !x.output) {
                    if let Some(to) = centre(&pin.inst) {
                        painter.line_segment(
                            [from, to],
                            Stroke::new(0.5, Color32::from_rgb(90, 200, 255)),
                        );
                        painter.circle_stroke(to, 1.2, Stroke::new(0.6, hi));
                    }
                }
            }
        }
    }
}
