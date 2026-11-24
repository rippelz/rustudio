//! The MCP button in the top bar (left of Collaborate) and its panel.
//!
//! The button shows the state at a glance: a grey dot when off, green when
//! listening, blue with a count when clients are connected, amber when a
//! call waits for the person, red when it couldn't listen. The panel turns
//! the server on, sets the port and token, the tool groups an agent may use
//! and which calls need the person's OK, gives the commands that connect
//! the claude CLI and other clients, and lists recent calls with their
//! results. Calls that need an OK open a small dialog over the editor.

use egui::{Color32, RichText, Sense, Vec2};
use rustudio_engine::theme;

use super::settings::{Confirm, Group, DEFAULT_PORT};
use crate::app::Editor;

/// The panel's pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Settings,
    Connect,
    Activity,
}

const GREEN: Color32 = Color32::from_rgb(72, 199, 116);

/// Where this rustudio binary is (for the stdio command).
fn exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "rustudio".into())
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub struct Snippets {
    pub claude_http: String,
    pub claude_stdio: String,
    pub json_http: String,
    pub json_stdio: String,
}

/// The connect commands / configs for a URL and token.
pub fn snippets(url: &str, token: Option<&str>) -> Snippets {
    let exe = exe();
    let header = token.map(|t| format!(" --header \"Authorization: Bearer {t}\"")).unwrap_or_default();
    let headers = token.map(|t| serde_json::json!({ "Authorization": format!("Bearer {t}") }));
    let mut http = serde_json::json!({ "type": "http", "url": url });
    if let Some(h) = headers {
        http["headers"] = h;
    }
    Snippets {
        claude_http: format!("claude mcp add --scope user --transport http rustudio {url}{header}"),
        claude_stdio: format!("claude mcp add --scope user rustudio -- {} mcp", shell_quote(&exe)),
        json_http: serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": { "rustudio": http } })).unwrap_or_default(),
        json_stdio: serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": { "rustudio": { "command": exe, "args": ["mcp"] } } })).unwrap_or_default(),
    }
}

impl Editor {
    /// One line of state for the button's tooltip and the panel's header.
    pub fn mcp_status_text(&self) -> (Color32, String) {
        if let Some(e) = &self.mcp.error {
            return (theme::ERROR, format!("Couldn't start: {e}"));
        }
        let Some(port) = self.mcp.port() else {
            return (theme::TEXT_DIM, "Off".into());
        };
        if !self.mcp.confirm.is_empty() {
            return (theme::WARN, format!("{} call(s) waiting for your OK", self.mcp.confirm.len()));
        }
        let clients = self.mcp.clients();
        if clients.is_empty() {
            (GREEN, format!("Listening on 127.0.0.1:{port} · no clients yet"))
        } else {
            let names: Vec<String> = clients.iter().map(|c| c.name.clone()).collect();
            (theme::SELECT, format!("Listening on 127.0.0.1:{port} · {} client(s): {}", clients.len(), names.join(", ")))
        }
    }

    /// The top bar's MCP button (drawn right to left, so it lands left of
    /// Collaborate).
    pub fn mcp_top_button(&mut self, ui: &mut egui::Ui) {
        let (col, text) = self.mcp_status_text();
        let clients = self.mcp.clients().len();
        let label = if clients > 0 { format!("MCP {clients}") } else { "MCP".into() };
        let font = egui::FontId::proportional(13.0);
        let galley = ui.painter().layout_no_wrap(label, font, theme::TEXT);
        let size = Vec2::new(galley.size().x + 26.0, 24.0);
        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
        let bg = if self.mcp.panel_open { theme::ACTIVE_TAB } else if resp.hovered() { theme::HOVER_BG } else { Color32::TRANSPARENT };
        ui.painter().rect_filled(r, 4.0, bg);
        ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, theme::DIVIDER), egui::StrokeKind::Inside);
        let dot = egui::pos2(r.min.x + 10.0, r.center().y);
        ui.painter().circle_filled(dot, 4.0, col);
        ui.painter().galley(egui::pos2(r.min.x + 18.0, r.center().y - galley.size().y / 2.0), galley, theme::TEXT);
        self.ui_rects.insert("mcp.button", r);
        if resp.on_hover_text(format!("MCP server: {text}")).clicked() {
            self.mcp.panel_open = !self.mcp.panel_open;
        }
    }

    /// The panel and the confirmation dialog.
    pub fn mcp_windows(&mut self, ctx: &egui::Context) {
        self.mcp_confirm_ui(ctx);
        if !self.mcp.panel_open {
            return;
        }
        let mut open = true;
        // Drops down from the button, its right edge on the button's.
        let at = self.ui_rects.get("mcp.button").map(|r| egui::pos2(r.max.x + 120.0, r.max.y + 6.0)).unwrap_or(egui::pos2(ctx.content_rect().right() - 12.0, 60.0));
        let at = egui::pos2(at.x.min(ctx.content_rect().right() - 8.0), at.y);
        egui::Window::new("MCP Server")
            .id(egui::Id::new("mcp_panel"))
            .open(&mut open)
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(at)
            .default_width(560.0)
            .default_height(760.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| self.mcp_panel(ui));
        self.mcp.panel_open = open;
        if let Some(r) = ctx.memory(|m| m.area_rect(egui::Id::new("mcp_panel"))) {
            self.ui_rects.insert("mcp.panel", r);
        }
    }

    fn mcp_panel(&mut self, ui: &mut egui::Ui) {
        let (col, text) = self.mcp_status_text();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, col);
            ui.label(RichText::new(text).strong());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (t, label) in [(Tab::Settings, "Settings".to_string()), (Tab::Connect, "Connect".to_string()), (Tab::Activity, format!("Activity ({})", self.mcp.activity.len()))] {
                if ui.selectable_label(self.mcp.tab == t, label).clicked() {
                    self.mcp.tab = t;
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.mcp.tab {
            Tab::Settings => self.mcp_settings_ui(ui),
            Tab::Connect => self.mcp_connect_ui(ui),
            Tab::Activity => self.mcp_activity_ui(ui),
        });
    }

    fn mcp_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.label(
            RichText::new("Lets an AI agent (the claude CLI, Cursor, …) drive this window while it stays open: read and edit the place, run Luau, Play, read Output, take screenshots, and work in a live Team Create session.")
                .color(theme::TEXT_DIM),
        );
        ui.add_space(6.0);
        let forced = std::env::var_os("RUSTUDIO_MCP").is_some_and(|v| !v.is_empty() && v != "0");
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut self.mcp.settings.enabled, RichText::new("Enable the MCP server").strong()).changed();
            if forced {
                ui.label(RichText::new("(on for this run: RUSTUDIO_MCP)").color(theme::TEXT_DIM));
            }
            if self.mcp.error.is_some() && ui.button("Retry").clicked() {
                self.mcp.retry();
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Transport").strong());
        egui::Grid::new("mcp_transport").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Kind");
            ui.label("Streamable HTTP, localhost only (127.0.0.1)");
            ui.end_row();
            ui.label("Port");
            ui.horizontal(|ui| {
                let mut port = self.mcp.settings.port;
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65535)).changed() {
                    self.mcp.settings.port = port;
                    changed = true;
                    self.mcp.retry();
                }
                if port != DEFAULT_PORT && ui.small_button("Default").clicked() {
                    self.mcp.settings.port = DEFAULT_PORT;
                    changed = true;
                    self.mcp.retry();
                }
                if let Some(p) = self.mcp.port() {
                    if self.mcp.env_port.is_some() {
                        ui.label(RichText::new(format!("using {p} for this run (RUSTUDIO_MCP)")).color(theme::TEXT_DIM));
                    } else if p != self.mcp.settings.port {
                        ui.label(RichText::new(format!("using {p} (the port was taken by another window)")).color(theme::TEXT_DIM));
                    }
                }
            });
            ui.end_row();
            ui.label("Token");
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut self.mcp.settings.require_token, "Required").changed();
                let shown = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("mcp_show_token"))).unwrap_or(false);
                let t = &self.mcp.settings.token;
                let text = if shown { t.clone() } else { format!("{}…", &t[..t.len().min(4)]) };
                ui.monospace(text);
                if ui.small_button(if shown { "Hide" } else { "Show" }).clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("mcp_show_token"), !shown));
                }
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(t.clone());
                }
                if ui.small_button("New token").on_hover_text("Clients configured with the old token stop working").clicked() {
                    self.mcp.settings.token = super::settings::new_token();
                    changed = true;
                }
            });
            ui.end_row();
        });
        if !self.mcp.settings.require_token {
            ui.colored_label(theme::WARN, "Without a token any program on this computer can drive rustudio.");
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Allowed tools").strong());
        for g in Group::ALL {
            let on = self.mcp.settings.groups.get_mut(g);
            let label = if g == Group::Live { RichText::new(g.label()).color(theme::WARN) } else { RichText::new(g.label()) };
            changed |= ui.checkbox(on, label).on_hover_text(g.hint()).changed();
            ui.label(RichText::new(g.hint()).small().color(theme::TEXT_DIM));
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Ask me before").strong());
        ui.horizontal(|ui| {
            for c in [Confirm::Never, Confirm::Live, Confirm::Writes] {
                changed |= ui.radio_value(&mut self.mcp.settings.confirm, c, c.label()).changed();
            }
        });
        if self.mcp.allow_all {
            ui.horizontal(|ui| {
                ui.colored_label(theme::WARN, "Every call is allowed until rustudio closes.");
                if ui.small_button("Ask again").clicked() {
                    self.mcp.allow_all = false;
                }
            });
        }

        if changed {
            self.mcp.save_settings();
        }
    }

    fn mcp_connect_ui(&mut self, ui: &mut egui::Ui) {
        let port = self.mcp.port().unwrap_or(self.mcp.settings.port);
        let url = format!("http://127.0.0.1:{port}/mcp");
        let token = self.mcp.settings.require_token.then_some(self.mcp.settings.token.as_str());
        let sn = snippets(&url, token);
        let copy_row = |ui: &mut egui::Ui, title: &str, hint: &str, text: &str| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong());
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(text.to_string());
                }
            });
            if !hint.is_empty() {
                ui.label(RichText::new(hint).small().color(theme::TEXT_DIM));
            }
            let mut t = text.to_string();
            ui.add(egui::TextEdit::multiline(&mut t).font(egui::TextStyle::Monospace).desired_rows(1).desired_width(f32::INFINITY));
        };
        copy_row(ui, "claude CLI, any open window (recommended)", "A stdio bridge: finds the rustudio windows with MCP on and talks to the newest (rustudio_use_window picks another).", &sn.claude_stdio);
        ui.horizontal(|ui| {
            if ui.button("Add to the claude CLI").on_hover_text(&sn.claude_stdio).clicked() {
                let r = std::process::Command::new("claude").args(["mcp", "add", "--scope", "user", "rustudio", "--", &exe(), "mcp"]).output();
                let msg = match r {
                    Ok(o) if o.status.success() => format!("Added: {}", String::from_utf8_lossy(&o.stdout).trim()),
                    Ok(o) => format!("claude said: {}{}", String::from_utf8_lossy(&o.stdout).trim(), String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("Couldn't run claude ({e}); copy the command instead"),
                };
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("mcp_add_result"), msg));
            }
            if let Some(m) = ui.ctx().data(|d| d.get_temp::<String>(egui::Id::new("mcp_add_result"))) {
                ui.label(RichText::new(m).small());
            }
        });
        ui.add_space(4.0);
        copy_row(ui, "claude CLI, this window over HTTP", "Talks to this window's port directly (each window has its own).", &sn.claude_http);
        ui.add_space(4.0);
        copy_row(ui, "Other clients (stdio JSON)", "Desktop apps, Cursor, …: in their MCP servers config.", &sn.json_stdio);
        ui.add_space(4.0);
        copy_row(ui, "Other clients (HTTP JSON)", "", &sn.json_http);
    }

    fn mcp_activity_ui(&mut self, ui: &mut egui::Ui) {
        if self.mcp.activity.is_empty() {
            ui.label(RichText::new("No calls yet.").color(theme::TEXT_DIM));
            return;
        }
        if ui.small_button("Clear").clicked() {
            self.mcp.activity.retain(|a| a.ok.is_none());
        }
        let mut answer = None;
        for a in self.mcp.activity.iter() {
            let (mark, col) = match a.ok {
                Some(true) => ("✔", GREEN),
                Some(false) => ("✖", theme::ERROR),
                None if self.mcp.confirm.iter().any(|(_, id, _)| *id == a.id) => ("?", theme::WARN),
                None => ("…", theme::TEXT_DIM),
            };
            let ms = a.ms.map(|m| format!("{m} ms")).unwrap_or_else(|| format!("{:.0} s", a.started.elapsed().as_secs_f32()));
            let head = format!("{}  {}  {}  ·  {}  ·  {ms}", a.at, mark, a.tool, a.client);
            let row = if a.ok == Some(false) { col } else { theme::TEXT };
            egui::CollapsingHeader::new(RichText::new(head).color(row).monospace())
                .id_salt(("mcp_act", a.id))
                .show(ui, |ui| {
                    ui.label(RichText::new("Arguments").small().color(theme::TEXT_DIM));
                    ui.label(RichText::new(&a.args).monospace().small());
                    if a.ok.is_some() {
                        ui.label(RichText::new("Result").small().color(theme::TEXT_DIM));
                        let mut r = a.result.clone();
                        if r.len() > 1500 {
                            r.truncate(1500);
                            r.push('…');
                        }
                        ui.label(RichText::new(r).monospace().small());
                    } else if self.mcp.confirm.iter().any(|(_, id, _)| *id == a.id) {
                        ui.horizontal(|ui| {
                            if ui.button("Allow").clicked() {
                                answer = Some((a.id, true));
                            }
                            if ui.button("Deny").clicked() {
                                answer = Some((a.id, false));
                            }
                        });
                    }
                });
        }
        if let Some((id, allow)) = answer {
            self.mcp_answer(id, allow);
        }
    }

    /// The oldest call waiting for an OK, over the editor.
    fn mcp_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some((job, act, at)) = self.mcp.confirm.front() else { return };
        let (tool, client, act) = (job.tool.clone(), job.client.clone(), *act);
        let mut args = serde_json::to_string_pretty(&job.args).unwrap_or_default();
        if args.len() > 4000 {
            args.truncate(4000);
            args.push('…');
        }
        let left = 120.0 - at.elapsed().as_secs_f32();
        let live = self.live_team_create_ids().is_some();
        let mut answer = None;
        egui::Window::new("Allow this MCP call?")
            .id(egui::Id::new("mcp_confirm"))
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("{client} wants to run {tool}"));
                if live {
                    ui.colored_label(theme::WARN, "This place is a live Team Create session: the change reaches every collaborator.");
                }
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.label(RichText::new(&args).monospace().small());
                });
                ui.label(RichText::new(format!("Declined by itself in {:.0} s; {} more waiting", left.max(0.0), self.mcp.confirm.len() - 1)).small().color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Allow").strong()).clicked() {
                        answer = Some(1);
                    }
                    if ui.button("Deny").clicked() {
                        answer = Some(0);
                    }
                    if ui.button("Allow all until rustudio closes").clicked() {
                        answer = Some(2);
                    }
                });
            });
        match answer {
            Some(2) => {
                self.mcp.allow_all = true;
                let ids: Vec<u64> = self.mcp.confirm.iter().map(|(_, id, _)| *id).collect();
                for id in ids {
                    self.mcp_answer(id, true);
                }
            }
            Some(a) => self.mcp_answer(act, a == 1),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn snippets_carry_the_url_and_token() {
        let s = super::snippets("http://127.0.0.1:44755/mcp", Some("abc"));
        assert!(s.claude_http.contains("--transport http rustudio http://127.0.0.1:44755/mcp --header \"Authorization: Bearer abc\""), "{}", s.claude_http);
        assert!(s.claude_stdio.ends_with(" mcp") && s.claude_stdio.contains("-- "));
        let j: serde_json::Value = serde_json::from_str(&s.json_http).unwrap();
        assert_eq!(j["mcpServers"]["rustudio"]["headers"]["Authorization"], "Bearer abc");
        let n = super::snippets("http://x/mcp", None);
        assert!(!n.claude_http.contains("--header"));
        assert_eq!(super::shell_quote("/a b/c"), "'/a b/c'");
    }
}
