//! Roblox account window (docs/AUTH.md): one sign-in, the session cookie from the
//! login keyring (imported once from ~/.config/rustudio/roblox-cookie).
//! Network and keyring work runs on background threads; results come back
//! over a channel and are polled each frame. The cookie never reaches this
//! module: it stays inside rustudio_account's Session.

use egui::RichText;
use rustudio_account::{CookieSource, SavedAccount};
use rustudio_engine::theme;

use crate::app::Editor;
use crate::account::client_snapshot;
pub use crate::account::SessionState;

#[path = "account_destination.rs"]
mod destination;
pub(crate) use destination::Picker;

impl Editor {
    pub fn account_window(&mut self, ctx: &egui::Context) {
        if self.game.account.open || self.cloud_upload_mode().is_some() {
            self.game.account.restore();
        }
        if self.game.account.take_changed() {
            self.cloud.forget_lists();
            self.start.forget_lists();
        }
        if self.game.account.open {
            self.game.account.heads(ctx);
            self.account_popover(ctx);
        }
        self.roblox_destination_window(ctx);
    }

    /// The account dropdown under the top-right account button.
    fn account_popover(&mut self, ctx: &egui::Context) {
        const W: f32 = 300.0;
        let button = self.ui_rects.get("account").copied();
        let screen = ctx.content_rect();
        let pos = match button {
            Some(r) => egui::pos2((r.max.x - W).max(screen.min.x + 8.0), r.max.y + 6.0),
            None => egui::pos2(screen.max.x - W - 8.0, screen.min.y + 40.0),
        };
        let frame = egui::Frame::new()
            .fill(theme::PANEL_DARK)
            .stroke(egui::Stroke::new(1.0, theme::DIVIDER))
            .corner_radius(6.0)
            .shadow(egui::epaint::Shadow {
                offset: [0, 6],
                blur: 18,
                spread: 0,
                color: egui::Color32::from_black_alpha(110),
            });
        let shown = egui::Popup::new(
            egui::Id::new("account_popover"), ctx.clone(), pos, egui::LayerId::background(),
        )
        .kind(egui::PopupKind::Menu)
        .align_alternatives(&[])
        .frame(frame)
        .show(|ui| {
            ui.set_width(W);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            self.account_contents(ui);
        })
        .expect("account popup is open");
        // Click outside (not on the button, which toggles) or Esc closes it.
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| {
                    !shown.response.rect.contains(p) && !button.is_some_and(|b| b.contains(p))
                })
        });
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if outside || esc {
            self.game.account.open = false;
            if let Some(q) = &self.game.account.quick {
                q.cancel();
            }
            self.game.account.adding = false;
            self.game.account.cookie_input.clear();
            self.game.account.add_error = None;
        }
    }

    fn account_contents(&mut self, ui: &mut egui::Ui) {
        enum Do {
            Switch(u64),
            Remove(u64),
            SignOut,
            Retry,
            BrowserSignIn,
        }
        let mut todo: Option<Do> = None;
        let a = &mut self.game.account;
        let accounts = a.accounts();
        let signed = a.signed_in();
        let checking = matches!(a.state, SessionState::Checking(_));
        let active = a.active_id();
        let me = active.and_then(|id| accounts.iter().find(|x| x.id == id).cloned());

        // Who is signed in.
        pad(ui, 14.0, 14.0, |ui| {
            ui.horizontal(|ui| {
                match (&me, signed || checking) {
                    (Some(m), true) => a.head_widget(ui, m.id, &m.name, 44.0),
                    _ => icon_plate(ui, "user", 44.0),
                }
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(3.0);
                    match (&me, signed, checking) {
                        (Some(m), true, _) => {
                            ui.label(RichText::new(disp(m)).size(15.0).strong().color(theme::TEXT));
                            ui.add_space(2.0);
                            ui.label(RichText::new(format!("@{}", m.name)).size(12.0).color(theme::TEXT_DIM));
                        }
                        (_, _, true) => {
                            let who = match &a.state {
                                SessionState::Checking(Some(w)) => w.clone(),
                                _ => "Signing in".into(),
                            };
                            ui.label(RichText::new(who).size(15.0).strong().color(theme::TEXT));
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(11.0).color(theme::TEXT_DIM));
                                ui.add_space(6.0);
                                ui.label(RichText::new("Checking with Roblox…").size(12.0).color(theme::TEXT_DIM));
                            });
                        }
                        _ => {
                            ui.label(RichText::new("Not signed in").size(15.0).strong().color(theme::TEXT));
                            ui.add_space(2.0);
                            ui.label(RichText::new("Sign in to open your places").size(12.0).color(theme::TEXT_DIM));
                        }
                    }
                });
            });
            if let SessionState::SignedOut(why) = &a.state {
                if why != "signed out" && !why.starts_with("signed out (") {
                    ui.add_space(10.0);
                    ui.label(RichText::new(why).size(12.0).color(theme::WARN));
                }
            }
        });
        divider(ui);

        if a.adding || (!signed && !checking && accounts.is_empty()) {
            // Add account: one field, one button.
            pad(ui, 14.0, 12.0, |ui| {
                ui.label(RichText::new("Add account").size(13.0).strong().color(theme::TEXT));
                ui.add_space(8.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut a.cookie_input)
                        .password(true)
                        .hint_text("Paste .ROBLOSECURITY cookie")
                        .margin(egui::vec2(8.0, 6.0))
                        .desired_width(f32::INFINITY),
                );
                if a.adding && a.cookie_input.is_empty() && !field.has_focus() && a.add_error.is_none() {
                    field.request_focus();
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("From a browser signed in to roblox.com")
                        .size(11.5)
                        .color(theme::TEXT_DIM),
                );
                if let Some(e) = &a.add_error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(e).size(12.0).color(theme::ERROR));
                }
                ui.add_space(10.0);
                if let Some(q) = &mut a.quick {
                    let code = q.code.lock().unwrap().clone();
                    let note = q.note.lock().unwrap().clone();
                    egui::Frame::new()
                        .fill(theme::PANEL_DARK)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical_centered(|ui| {
                                match &code {
                                    Some(c) => {
                                        ui.label(RichText::new("Enter this code on Roblox").size(12.0).color(theme::TEXT_DIM));
                                        ui.add_space(4.0);
                                        ui.label(RichText::new(c).size(26.0).monospace().strong().color(theme::TEXT));
                                        ui.add_space(4.0);
                                        if ui.link(RichText::new("Open the page again").size(11.5)).clicked() {
                                            q.opened = false;
                                        }
                                    }
                                    None => {
                                        ui.add(egui::Spinner::new().size(14.0).color(theme::TEXT_DIM));
                                    }
                                }
                                ui.add_space(4.0);
                                ui.label(RichText::new(note).size(11.5).color(theme::TEXT_DIM));
                            });
                        });
                    if code.is_some() && !q.opened {
                        q.opened = true;
                        ui.ctx().open_url(egui::OpenUrl::new_tab(rustudio_account::quick_login::CONFIRM_URL));
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                } else {
                    let browser = ui.add_enabled(
                        !a.busy(),
                        egui::Button::new(RichText::new("Sign in with your browser").color(theme::TEXT))
                            .min_size(egui::vec2(ui.available_width(), 26.0)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Shows a code to confirm on roblox.com, in the browser you're signed in to")
                            .size(11.5)
                            .color(theme::TEXT_DIM),
                    );
                    if browser.clicked() {
                        todo = Some(Do::BrowserSignIn);
                    }
                }
                ui.add_space(10.0);
                let mut go = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let row = egui::vec2(ui.available_width(), 26.0);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let ready = !a.busy() && !a.cookie_input.trim().is_empty();
                    let label = if a.busy() { "Signing in…" } else { "Sign in" };
                    go |= ui
                        .add_enabled(
                            ready,
                            egui::Button::new(RichText::new(label).color(egui::Color32::WHITE))
                                .fill(theme::ACCENT)
                                .min_size(egui::vec2(76.0, 26.0)),
                        )
                        .clicked();
                    if (signed || !accounts.is_empty() || a.quick.is_some())
                        && ui
                            .add(egui::Button::new("Cancel").fill(egui::Color32::TRANSPARENT).min_size(egui::vec2(64.0, 26.0)))
                            .clicked()
                    {
                        if let Some(q) = &a.quick {
                            q.cancel();
                        }
                        a.adding = false;
                        a.cookie_input.clear();
                        a.add_error = None;
                    }
                });
                if go && !a.busy() && !a.cookie_input.trim().is_empty() {
                    a.add_account();
                }
            });
        } else {
            // Other accounts: click to switch.
            let others: Vec<&SavedAccount> =
                accounts.iter().filter(|x| Some(x.id) != active || !(signed || checking)).collect();
            if !others.is_empty() {
                ui.add_space(6.0);
                for x in &others {
                    let busy = a.busy();
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 42.0), egui::Sense::click());
                    let hover = resp.hovered() && !busy;
                    if hover {
                        ui.painter().rect_filled(r.shrink2(egui::vec2(6.0, 1.0)), 4.0, theme::HOVER_BG);
                    }
                    let head = egui::Rect::from_min_size(egui::pos2(r.min.x + 16.0, r.center().y - 14.0), egui::vec2(28.0, 28.0));
                    a.paint_head(ui, head, x.id, &x.name);
                    let tx = head.max.x + 12.0;
                    ui.painter().text(egui::pos2(tx, r.center().y - 7.0), egui::Align2::LEFT_CENTER, disp(x), egui::FontId::proportional(13.0), theme::TEXT);
                    ui.painter().text(egui::pos2(tx, r.center().y + 8.0), egui::Align2::LEFT_CENTER, format!("@{}", x.name), egui::FontId::proportional(11.5), theme::TEXT_DIM);
                    let xr = egui::Rect::from_center_size(egui::pos2(r.max.x - 26.0, r.center().y), egui::vec2(22.0, 22.0));
                    let mut on_x = false;
                    if a.confirm_remove == Some(x.id) {
                        let rr = egui::Rect::from_min_max(egui::pos2(r.max.x - 92.0, r.center().y - 12.0), egui::pos2(r.max.x - 14.0, r.center().y + 12.0));
                        let b = ui.put(rr, egui::Button::new(RichText::new("Remove").size(12.0).color(egui::Color32::WHITE)).fill(theme::ERROR.gamma_multiply(0.8)));
                        if b.clicked() {
                            todo = Some(Do::Remove(x.id));
                        }
                        on_x = true;
                    } else if hover || resp.hovered() {
                        let xh = ui.interact(xr, ui.id().with(("remove", x.id)), egui::Sense::click());
                        if xh.hovered() {
                            ui.painter().rect_filled(xr, 4.0, theme::ACTIVE_TAB);
                        }
                        rustudio_engine::ti::paint(ui.painter(), xr.shrink(4.0), "x", if xh.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                        if xh.clicked() {
                            a.confirm_remove = Some(x.id);
                        }
                        on_x = xh.hovered();
                        xh.on_hover_text("Remove from this computer");
                    }
                    if resp.clicked() && !on_x && !busy {
                        todo = Some(Do::Switch(x.id));
                    }
                }
                if let Some(e) = &a.add_error {
                    pad(ui, 16.0, 4.0, |ui| {
                        ui.label(RichText::new(e).size(12.0).color(theme::ERROR));
                    });
                }
                ui.add_space(2.0);
            } else {
                ui.add_space(6.0);
            }
            if pop_row(ui, "user-plus", "Add account", !a.busy()).clicked() {
                a.adding = true;
                a.add_error = None;
                a.confirm_remove = None;
            }
            ui.add_space(6.0);
            divider(ui);
            ui.add_space(6.0);
            if signed {
                if pop_row(ui, "logout", "Sign out", !a.busy()).clicked() {
                    todo = Some(Do::SignOut);
                }
            } else if pop_row(ui, "refresh", "Check the session again", !a.busy()).clicked() {
                todo = Some(Do::Retry);
            }
            ui.add_space(6.0);
        }

        // Footer: the account's facts in one quiet line.
        if signed {
            divider(ui);
            pad(ui, 14.0, 8.0, |ui| {
                let avatar = match a.avatar() {
                    Some(l) => if l.r15 { "R15 avatar" } else { "R6 avatar" },
                    None => "default body",
                };
                let id = active.map(|i| format!("ID {i}")).unwrap_or_default();
                let place = match a.source {
                    Some(CookieSource::Keyring) | Some(CookieSource::File { imported: true }) => "login keyring",
                    _ => "cookie file",
                };
                ui.label(RichText::new(format!("{id}  ·  {avatar}  ·  {place}")).size(11.0).color(theme::TEXT_DIM));
            });
        }

        match todo {
            Some(Do::Switch(id)) => {
                a.confirm_remove = None;
                a.switch_to(id);
            }
            Some(Do::Remove(id)) => {
                a.confirm_remove = None;
                a.remove_account(id);
            }
            Some(Do::SignOut) => a.sign_out(),
            Some(Do::Retry) => a.sign_in(),
            Some(Do::BrowserSignIn) => a.browser_sign_in(),
            None => {}
        }
    }

}

fn disp(a: &SavedAccount) -> &str {
    if a.display_name.is_empty() { &a.name } else { &a.display_name }
}

/// A section with side and vertical padding.
fn pad(ui: &mut egui::Ui, x: f32, y: f32, f: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(x as i8, y as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        f(ui);
    });
}

fn divider(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::DIVIDER);
}

/// A round plate with an icon (no headshot).
fn icon_plate(ui: &mut egui::Ui, icon: &str, size: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), size * 0.5, theme::PANEL_ALT);
    rustudio_engine::ti::paint(ui.painter(), r.shrink(size * 0.25), icon, theme::TEXT_DIM);
}

/// A full-width menu row: icon and label, hover highlight.
fn pop_row(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r.shrink2(egui::vec2(6.0, 1.0)), 4.0, theme::HOVER_BG);
    }
    let c = if enabled { theme::TEXT } else { theme::TEXT_DIM };
    let ir = egui::Rect::from_center_size(egui::pos2(r.min.x + 30.0, r.center().y), egui::vec2(17.0, 17.0));
    rustudio_engine::ti::paint(ui.painter(), ir, icon, if enabled { theme::TEXT_DIM } else { theme::DIVIDER });
    ui.painter().text(egui::pos2(r.min.x + 52.0, r.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(13.0), c);
    resp.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}
