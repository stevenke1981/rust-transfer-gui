use super::*;
use eframe::App;

fn draw(app: &mut TransferApp, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(720.0, 520.0))),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
    output.textures_delta.clear();
    output
}

fn text_position(output: &egui::FullOutput, expected: &str) -> Option<egui::Pos2> {
    output.shapes.iter().find_map(|shape| {
        if let egui::epaint::Shape::Text(text) = &shape.shape
            && text.galley.job.text == expected
        {
            return Some(text.pos + text.galley.rect.center().to_vec2());
        }
        None
    })
}

#[test]
fn renders_each_language_and_switches_on_the_same_context() {
    let ctx = egui::Context::default();
    let mut app = TransferApp::with_config(ctx.clone(), Config::default());
    for language in Language::ALL {
        app.config.settings.language = language;
        draw(&mut app, &ctx, Vec::new());
        let output = draw(&mut app, &ctx, Vec::new());
        for label in ["Session", "Tools", "View", "Help", "➕  Start new session"] {
            assert!(text_position(&output, language.text(label)).is_some(), "{}: {label}", language.code());
        }
        app.open_settings();
        draw(&mut app, &ctx, Vec::new());
        let output = draw(&mut app, &ctx, Vec::new());
        assert!(text_position(&output, language.text("Apply")).is_some());
        app.show_settings = false;
    }
}

#[test]
fn cancel_discards_draft_without_changing_active_settings() {
    let ctx = egui::Context::default();
    let mut app = TransferApp::with_config(ctx.clone(), Config::default());
    let original = app.config.settings.clone();
    app.open_settings();
    app.settings_draft.language = Language::Japanese;
    app.settings_draft.dark_mode = !original.dark_mode;
    app.settings_draft.show_sidebar = !original.show_sidebar;
    draw(&mut app, &ctx, Vec::new());
    let output = draw(&mut app, &ctx, Vec::new());
    let position = text_position(&output, original.language.text("Cancel")).expect("Cancel button");
    for pressed in [true, false] {
        draw(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    assert!(!app.show_settings);
    assert_eq!(app.config.settings, original);
    assert_eq!(Language::from_context(&ctx), original.language);
    app.open_settings();
    assert_eq!(app.settings_draft, original);
}

#[test]
fn apply_saves_languages_and_keeps_sessions_across_reload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.conf");
    let ctx = egui::Context::default();
    let mut config = Config::default();
    let mut session = SavedSession::new(Protocol::Tftp);
    session.host = "127.0.0.1".into();
    session.name = "測試連線 — 日本語".into();
    config.sessions.push(session);
    let mut app = TransferApp::with_config(ctx.clone(), config);
    for language in Language::ALL {
        app.open_settings();
        app.settings_draft.language = language;
        app.apply_settings(&ctx, |candidate| {
            std::fs::write(&path, candidate.to_text())?;
            Ok(path.clone())
        });
        assert!(!app.show_settings);
        assert_eq!(Language::from_context(&ctx), language);
        let reloaded = Config::from_text(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(reloaded, app.config);
        assert_eq!(reloaded.settings.language, language);
        assert_eq!(reloaded.sessions[0].name, "測試連線 — 日本語");
    }
}

#[test]
fn failed_apply_keeps_active_settings_and_editable_draft() {
    let ctx = egui::Context::default();
    let mut app = TransferApp::with_config(ctx.clone(), Config::default());
    let original = app.config.clone();
    app.open_settings();
    app.settings_draft.language = Language::Japanese;
    app.apply_settings(&ctx, |_| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only")));
    assert!(app.show_settings);
    assert_eq!(app.config, original);
    assert_eq!(Language::from_context(&ctx), original.settings.language);
    assert_eq!(app.settings_draft.language, Language::Japanese);
    assert!(matches!(app.notice, Some((Level::Error, _))));
}
