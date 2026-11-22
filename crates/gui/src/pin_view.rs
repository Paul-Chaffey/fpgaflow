//! The Pins tab: a pin planner on the package's ball grid (top view, A1 at
//! the top left).
//!
//! Select a port bit on the left, click a ball to put it there. Every edit
//! goes through the same checks as `fpgaflow pins set` (reserved pins,
//! configuration/power balls, pins already in use), sets the bank's I/O
//! standard when the board profile knows the bank voltage, and writes the
//! project's UCF in place.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use fpgaflow_flow::layout_model::pins::{self, Assignment, Issue, Severity};
use fpgaflow_flow::PinData;

/// Scene units between ball centres.
const P: f32 = 26.0;

const BANK_COLOURS: [Color32; 8] = [
    Color32::from_rgb(78, 121, 167),
    Color32::from_rgb(242, 142, 43),
    Color32::from_rgb(89, 161, 79),
    Color32::from_rgb(176, 122, 161),
    Color32::from_rgb(118, 183, 178),
    Color32::from_rgb(237, 201, 72),
    Color32::from_rgb(225, 87, 89),
    Color32::from_rgb(156, 117, 95),
];

pub enum PinRequest {
    Load,
    Open(PathBuf, u32),
}

pub struct PinView {
    pub data: Option<Arc<PinData>>,
    pub loading: bool,
    pub error: Option<String>,
    assigned: Vec<Assignment>,
    issues: Vec<Issue>,
    selected: Option<String>,
    message: Option<(bool, String)>,
    scene_rect: Rect,
}

impl Default for PinView {
    fn default() -> Self {
        PinView {
            data: None,
            loading: false,
            error: None,
            assigned: Vec::new(),
            issues: Vec::new(),
            selected: None,
            message: None,
            scene_rect: Rect::ZERO,
        }
    }
}

impl PinView {
    pub fn clear(&mut self) {
        *self = PinView::default();
    }

    pub fn set_data(&mut self, r: Result<Arc<PinData>, String>) {
        self.loading = false;
        match r {
            Ok(d) => {
                self.data = Some(d);
                self.error = None;
                self.scene_rect = Rect::ZERO;
                self.refresh();
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Re-read the UCFs (after an edit here or in the editor).
    pub fn refresh(&mut self) {
        if let Some(d) = &self.data {
            self.assigned = d.assignments();
            self.issues = d.issues(&self.assigned);
        }
    }

    pub fn watches(&self, path: &std::path::Path) -> bool {
        self.data
            .as_ref()
            .is_some_and(|d| d.files.iter().any(|f| f == path))
    }

    fn pin_of(&self, port: &str) -> Option<&Assignment> {
        self.assigned
            .iter()
            .find(|a| pins::normalise(&a.net) == port)
    }

    fn net_on(&self, ball: &str) -> Option<&Assignment> {
        self.assigned
            .iter()
            .find(|a| a.pin.eq_ignore_ascii_case(ball))
    }

    fn assign(&mut self, ball: &str) {
        let (Some(d), Some(port)) = (self.data.clone(), self.selected.clone()) else {
            return;
        };
        if let Err(why) = pins::can_assign(&port, ball, &self.assigned, &d.package, d.board) {
            self.message = Some((false, why));
            return;
        }
        let standard = d
            .package
            .pin(ball)
            .and_then(|p| p.bank)
            .and_then(|b| d.board.and_then(|bd| bd.bank_standard(b)));
        self.message = Some(
            match pins::set_pin(&self.assigned, &d.new_file, &port, ball, standard) {
                Ok(f) => (
                    true,
                    format!(
                        "{port} -> {ball}{} in {}",
                        standard.map(|s| format!(" ({s})")).unwrap_or_default(),
                        f.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
                Err(e) => (false, format!("writing the UCF: {e}")),
            },
        );
        self.refresh();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PinRequest> {
        let mut req = None;
        if self.data.is_none() && !self.loading && self.error.is_none() {
            req = Some(PinRequest::Load);
        }
        let top_down = egui::Layout::top_down(egui::Align::LEFT);
        ui.horizontal_top(|ui| {
            let h = ui.available_height();
            ui.allocate_ui_with_layout(Vec2::new(300.0, h), top_down, |ui| {
                ui.set_width(300.0);
                ui.set_min_height(h);
                if let Some(r) = self.side(ui) {
                    req = Some(r);
                }
            });
            ui.separator();
            let rest = ui.available_size();
            ui.allocate_ui_with_layout(rest, top_down, |ui| self.grid(ui));
        });
        req
    }

    fn side(&mut self, ui: &mut egui::Ui) -> Option<PinRequest> {
        let mut req = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.loading, egui::Button::new("Reload"))
                .on_hover_text("Re-read the ports (Yosys) and the UCFs")
                .clicked()
            {
                req = Some(PinRequest::Load);
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
        let Some(d) = self.data.clone() else {
            return req;
        };
        ui.label(RichText::new(&d.part).strong());
        if let Some(b) = d.board {
            ui.small(&b.name);
        }
        if let Some((ok, m)) = &self.message {
            let c = if *ok {
                Color32::from_rgb(60, 170, 90)
            } else {
                Color32::from_rgb(220, 80, 70)
            };
            ui.colored_label(c, m);
        }
        ui.separator();
        ui.label(RichText::new("Ports").strong())
            .on_hover_text("Select a port bit, then click a ball to assign it");
        let issue_pins: Vec<String> = self
            .issues
            .iter()
            .filter(|i| i.severity() == Severity::Error)
            .filter_map(|i| i.pin().map(String::from))
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("pin_ports")
            .max_height(ui.available_height() * 0.6)
            .show(ui, |ui| {
                egui::Grid::new("ports")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        let mut select = None;
                        for p in &d.ports {
                            let a = self.pin_of(&p.name);
                            let sel = self.selected.as_deref() == Some(&p.name);
                            let bad = a.is_none_or(|a| issue_pins.contains(&a.pin));
                            let mut text = RichText::new(&p.name).monospace();
                            if bad {
                                text = text.color(Color32::from_rgb(220, 80, 70));
                            }
                            if ui.selectable_label(sel, text).clicked() {
                                select = Some(if sel { None } else { Some(p.name.clone()) });
                            }
                            ui.monospace(a.map(|a| a.pin.as_str()).unwrap_or("—"));
                            ui.small(
                                a.and_then(|a| d.labels.get(&a.pin))
                                    .map(String::as_str)
                                    .unwrap_or(&p.direction),
                            );
                            ui.end_row();
                        }
                        if let Some(s) = select {
                            self.selected = s;
                        }
                    });
            });
        if let Some(port) = self.selected.clone() {
            ui.horizontal(|ui| {
                if let Some(a) = self.pin_of(&port).cloned() {
                    if ui.button("Clear pin").clicked() {
                        self.message = Some(match pins::clear_pin(&self.assigned, &port) {
                            Ok(_) => (true, format!("{port}: pin cleared")),
                            Err(e) => (false, e.to_string()),
                        });
                        self.refresh();
                    }
                    if ui.button("Open UCF").clicked() {
                        req = Some(PinRequest::Open(a.file.clone(), a.line));
                    }
                } else if ui.button("Open UCF").clicked() {
                    req = Some(PinRequest::Open(d.new_file.clone(), 1));
                }
            });
        }

        ui.separator();
        let errors = self
            .issues
            .iter()
            .filter(|i| i.severity() == Severity::Error)
            .count();
        ui.label(
            RichText::new(format!(
                "Problems ({errors} error(s), {} warning(s))",
                self.issues.len() - errors
            ))
            .strong(),
        );
        egui::ScrollArea::vertical()
            .id_salt("pin_issues")
            .show(ui, |ui| {
                for i in &self.issues {
                    let c = match i.severity() {
                        Severity::Error => Color32::from_rgb(220, 80, 70),
                        Severity::Warning => Color32::from_rgb(200, 170, 60),
                    };
                    ui.colored_label(c, i.to_string());
                }
            });
        req
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.data.clone() else { return };
        let pkg = &d.package;
        let dark = ui.visuals().dark_mode;
        let text = ui.visuals().text_color();
        let extent = Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(
                (pkg.cols as f32 + 1.0) * P,
                (pkg.rows.len() as f32 + 2.5) * P,
            ),
        );
        let centre =
            |row: usize, col: usize| Pos2::new((col as f32 + 1.0) * P, (row as f32 + 1.0) * P);
        let bank_col = |b: u32| BANK_COLOURS[b as usize % BANK_COLOURS.len()];
        let selected_pin = self
            .selected
            .as_deref()
            .and_then(|s| self.pin_of(s))
            .map(|a| a.pin.clone());
        let mut clicked: Option<String> = None;
        let mut rect = self.scene_rect;
        egui::Scene::new()
            .zoom_range(0.1..=8.0)
            .show(ui, &mut rect, |ui| {
                let resp = ui.allocate_rect(extent, Sense::click());
                let hover = resp.hover_pos();
                let painter = ui.painter();
                let font = FontId::proportional(P * 0.3);
                for (c, label) in (1..=pkg.cols).map(|c| (c, c.to_string())) {
                    painter.text(
                        centre(0, c as usize - 1) - Vec2::new(0.0, P * 0.75),
                        Align2::CENTER_CENTER,
                        label,
                        font.clone(),
                        text,
                    );
                }
                for (r, label) in pkg.rows.iter().enumerate() {
                    painter.text(
                        centre(r, 0) - Vec2::new(P * 0.75, 0.0),
                        Align2::CENTER_CENTER,
                        label,
                        font.clone(),
                        text,
                    );
                }
                let mut hovered: Option<String> = None;
                for pin in &pkg.pins {
                    let Some((r, c)) = pkg.position(&pin.ball) else {
                        continue;
                    };
                    let p = centre(r, c);
                    let rad = P * 0.36;
                    let reserved = d.board.and_then(|b| b.reserved_reason(&pin.ball));
                    let fill = if pin.is_user_io() {
                        pin.bank
                            .map(bank_col)
                            .unwrap_or(Color32::GRAY)
                            .gamma_multiply(0.45)
                    } else if pin.function == "GND" {
                        if dark {
                            Color32::from_gray(20)
                        } else {
                            Color32::from_gray(60)
                        }
                    } else if pin.is_power() {
                        Color32::from_rgb(120, 40, 40)
                    } else {
                        Color32::from_gray(if dark { 90 } else { 170 })
                    };
                    painter.circle_filled(p, rad, fill);
                    if pin.is_gclk() {
                        painter.circle_stroke(
                            p,
                            rad + 1.5,
                            Stroke::new(1.0, Color32::from_rgb(255, 230, 60)),
                        );
                    }
                    if let Some(a) = self.net_on(&pin.ball) {
                        let col = pin.bank.map(bank_col).unwrap_or(Color32::GRAY);
                        painter.circle_filled(p, rad, col);
                        painter.text(
                            p,
                            Align2::CENTER_CENTER,
                            short(&a.net),
                            FontId::proportional(P * 0.22),
                            Color32::BLACK,
                        );
                    }
                    if reserved.is_some() {
                        let k = rad * 0.7;
                        let s = Stroke::new(1.5, Color32::from_rgb(230, 60, 50));
                        painter.line_segment([p + Vec2::new(-k, -k), p + Vec2::new(k, k)], s);
                        painter.line_segment([p + Vec2::new(-k, k), p + Vec2::new(k, -k)], s);
                    }
                    if selected_pin.as_deref() == Some(pin.ball.as_str()) {
                        painter.circle_stroke(p, rad + 3.0, Stroke::new(2.0, Color32::WHITE));
                    }
                    if hover.is_some_and(|h| h.distance(p) <= rad) {
                        painter.circle_stroke(p, rad + 1.0, Stroke::new(1.5, text));
                        hovered = Some(pin.ball.clone());
                    }
                }
                // Bank legend under the grid.
                let banks = pkg.banks();
                let per_row = 4;
                for (k, b) in banks.iter().enumerate() {
                    let x = P + (k % per_row) as f32 * 4.2 * P;
                    let y = (pkg.rows.len() as f32 + 0.9 + (k / per_row) as f32 * 0.8) * P;
                    painter.circle_filled(Pos2::new(x, y), P * 0.25, bank_col(*b));
                    let v = d
                        .board
                        .and_then(|bd| bd.bank_voltage(*b))
                        .map(|v| format!(" {v} V"))
                        .unwrap_or_default();
                    painter.text(
                        Pos2::new(x + P * 0.4, y),
                        Align2::LEFT_CENTER,
                        format!("bank {b}{v}"),
                        font.clone(),
                        text,
                    );
                }
                if let Some(h) = &hovered {
                    let pin = pkg.pin(h).unwrap();
                    let mut tip = format!("{} {}", pin.ball, pin.function);
                    if let Some(b) = pin.bank {
                        tip.push_str(&format!("\nbank {b}"));
                        if let Some(bd) = d.board {
                            if let (Some(v), Some(s)) = (bd.bank_voltage(b), bd.bank_standard(b)) {
                                tip.push_str(&format!(", {v} V ({s})"));
                            }
                        }
                    }
                    if let Some(l) = d.labels.get(h) {
                        tip.push_str(&format!("\nboard: {l}"));
                    }
                    if let Some(a) = self.net_on(h) {
                        tip.push_str(&format!("\nassigned: {}", a.net));
                    }
                    if let Some(r) = d.board.and_then(|b| b.reserved_reason(h)) {
                        tip.push_str(&format!("\nreserved: {r}"));
                    }
                    if resp.clicked() {
                        clicked = Some(h.clone());
                    }
                    resp.on_hover_text_at_pointer(tip);
                }
            });
        self.scene_rect = rect;
        if let Some(ball) = clicked {
            if self.selected.is_some() {
                self.assign(&ball);
            } else {
                // No port selected: select the one on this ball, if any.
                self.selected = self.net_on(&ball).map(|a| pins::normalise(&a.net));
            }
        }
    }
}

/// Short label for inside a ball: `seg0<3>` -> `seg0.3`, long names clipped.
fn short(net: &str) -> String {
    let n = net.replace('<', ".").replace('>', "");
    if n.chars().count() > 7 {
        n.chars().take(6).collect::<String>() + "…"
    } else {
        n
    }
}
