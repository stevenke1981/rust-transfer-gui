use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui::{self, RichText};

use super::{TransferApp, files, theme};
use crate::config::Protocol;
use crate::i18n::Language;
use crate::worker::{Level, SessionCommand};

pub(super) struct PendingUpload {
    pub tab_id: u64,
    pub directory: String,
    pub destination: String,
    pub paths: Vec<PathBuf>,
}

enum DropMode {
    Upload,
    Tftp,
}

fn local_files(dropped: &[egui::DroppedFileHandle], language: Language) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let mut names = HashSet::new();
    for file in dropped {
        let path = file.path();
        if !path.is_absolute() {
            return Err(language.text("Only local files are supported; folders cannot be uploaded.").into());
        }
        let metadata = path.metadata().map_err(|_| {
            language.format("Cannot read dropped file: {path}", &[("path", &path.display().to_string())])
        })?;
        if !metadata.is_file() {
            return Err(language.text("Only local files are supported; folders cannot be uploaded.").into());
        }
        if paths.contains(&path.to_path_buf()) {
            continue;
        }
        if !names.insert(files::file_name_of(path)) {
            return Err(language.text("Dropped files must have distinct destination names.").into());
        }
        paths.push(path.to_path_buf());
    }
    Ok(paths)
}

impl TransferApp {
    fn drop_mode(&self) -> Result<DropMode, &'static str> {
        if self.dialog.open || self.show_settings || self.pending_delete.is_some() || self.pending_upload.is_some() {
            return Err("Close the current dialog before dropping files.");
        }
        let Some(tab) = self.tabs.iter().find(|tab| Some(tab.id) == self.active) else {
            return Err("Open a session before dropping files.");
        };
        if tab.info.protocol == Protocol::Tftp && !tab.is_busy() {
            return Ok(DropMode::Tftp);
        }
        if !tab.is_idle() || tab.worker.is_none() {
            return Err("Wait until the session is connected and idle.");
        }
        Ok(DropMode::Upload)
    }

    pub(super) fn handle_file_drop(&mut self, ctx: &egui::Context) {
        let language = self.config.settings.language;
        let instruction = match self.drop_mode() {
            Ok(DropMode::Upload) => "Drop files here to upload to the active session.",
            Ok(DropMode::Tftp) => "Drop one file to prepare a TFTP upload.",
            Err(message) => message,
        };
        if ctx.input(|input| !input.raw.hovered_files.is_empty()) {
            egui::Area::new(egui::Id::new("file_drop_overlay"))
                .order(egui::Order::Tooltip)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(20.0).show(ui, |ui| {
                        ui.label(RichText::new(language.text(instruction)).size(20.0).color(theme::ACCENT_LIGHT));
                    });
                });
        }
        let dropped = ctx.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        if !dropped.is_empty() {
            self.receive_files(&dropped);
        }
    }

    fn receive_files(&mut self, dropped: &[egui::DroppedFileHandle]) {
        let language = self.config.settings.language;
        if let Err(message) = self.drop_mode() {
            self.notice = Some((Level::Error, language.text(message).into()));
            return;
        }
        let paths = match local_files(dropped, language) {
            Ok(paths) if !paths.is_empty() => paths,
            Ok(_) => return,
            Err(error) => {
                self.notice = Some((Level::Error, error));
                return;
            }
        };
        let Some(tab) = self.active_tab() else { return };
        if tab.info.protocol == Protocol::Tftp {
            if paths.len() != 1 {
                self.notice = Some((Level::Error, language.text("TFTP accepts one file at a time.").into()));
                return;
            }
            tab.tftp.local = paths[0].to_string_lossy().into_owned();
            if tab.tftp.remote.trim().is_empty() {
                tab.tftp.remote = files::file_name_of(&paths[0]);
            }
            self.notice = Some((
                Level::Info,
                language.text("TFTP file selected. Review the remote name, then click Put (upload).").into(),
            ));
            return;
        }
        self.pending_upload = Some(PendingUpload {
            tab_id: tab.id,
            directory: tab.browser.cwd.clone(),
            destination: format!(
                "{} {}:{} — {}",
                tab.info.protocol.label(),
                tab.info.host,
                tab.info.port,
                tab.browser.cwd
            ),
            paths,
        });
    }

    pub(super) fn upload_confirmation(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_upload else { return };
        let language = self.config.settings.language;
        let ready = self.tabs.iter().any(|tab| {
            tab.id == pending.tab_id && tab.is_idle() && tab.worker.is_some() && tab.browser.cwd == pending.directory
        });
        let mut upload = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("confirm_upload_drop")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading(language.text("Upload dropped files"));
            ui.label(language.text("Destination"));
            ui.monospace(&pending.destination);
            ui.label(language.format("{count} files selected", &[("count", &pending.paths.len().to_string())]));
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for path in &pending.paths {
                    ui.label(path.display().to_string());
                }
            });
            ui.weak(language.text("Existing remote files with the same names may be overwritten."));
            if !ready {
                ui.colored_label(
                    theme::WARN_YELLOW,
                    language.text("The destination changed or disconnected. Drop the files again."),
                );
            }
            ui.horizontal(|ui| {
                upload = ui.add_enabled(ready, egui::Button::new(language.text("⤴ Upload"))).clicked();
                cancel = ui.button(language.text("Cancel")).clicked();
            });
        });
        if upload {
            self.submit_dropped_upload();
        } else if cancel || modal.should_close() {
            self.pending_upload = None;
        }
    }

    fn submit_dropped_upload(&mut self) {
        let Some(pending) = self.pending_upload.take() else { return };
        let language = self.config.settings.language;
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.id == pending.tab_id && tab.is_idle() && tab.browser.cwd == pending.directory)
        else {
            self.notice = Some((
                Level::Error,
                language.text("The destination changed or disconnected. Drop the files again.").into(),
            ));
            return;
        };
        let mut queued = 0;
        for local in pending.paths {
            let remote_name = files::file_name_of(&local);
            if !tab.worker.as_ref().is_some_and(|worker| worker.send(SessionCommand::Upload { local, remote_name })) {
                self.notice = Some((
                    Level::Error,
                    language.format(
                        "The connection closed after queuing {count} files.",
                        &[("count", &queued.to_string())],
                    ),
                ));
                return;
            }
            queued += 1;
        }
        self.notice =
            Some((Level::Info, language.format("Queued {count} files for upload.", &[("count", &queued.to_string())])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, mpsc};

    use crate::app::session::Secrets;
    use crate::config::{Config, SavedSession};
    use crate::worker::{Event, WorkerHandle};
    use eframe::App;

    #[derive(Debug)]
    struct TestFile(PathBuf);

    impl egui::DroppedFile for TestFile {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|error| error.to_string())
        }
    }

    fn dropped(path: &Path) -> egui::DroppedFileHandle {
        Arc::new(TestFile(path.to_path_buf()))
    }

    fn test_app(ctx: &egui::Context) -> TransferApp {
        let mut app = TransferApp::with_config(ctx.clone(), Config::default());
        let mut session = SavedSession::new(Protocol::Tftp);
        session.host = "127.0.0.1".into();
        app.open_session(session, Secrets::default());
        app
    }

    fn connect_test_worker(
        app: &mut TransferApp,
        protocol: Protocol,
    ) -> (mpsc::Receiver<SessionCommand>, mpsc::Sender<Event>) {
        let (worker, receiver, events) = WorkerHandle::test_channel();
        let tab = &mut app.tabs[0];
        tab.info.protocol = protocol;
        tab.connected = true;
        tab.worker = Some(worker);
        tab.browser.cwd = "/uploads".into();
        (receiver, events)
    }

    #[test]
    fn native_drop_event_prepares_tftp_without_sending_and_keeps_remote_name() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("繁體檔案.txt");
        std::fs::write(&path, b"test").unwrap();
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        for remote in ["", "existing-remote.bin"] {
            app.tabs[0].tftp.remote = remote.into();
            let input = egui::RawInput { dropped_files: vec![dropped(&path)], ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
            output.textures_delta.clear();
            assert_eq!(app.tabs[0].tftp.local, path.to_string_lossy());
            assert_eq!(app.tabs[0].tftp.remote, if remote.is_empty() { "繁體檔案.txt" } else { remote });
            assert!(app.tabs[0].tftp.events.is_none());
            assert!(app.pending_upload.is_none());
        }
    }

    #[test]
    fn multiple_files_require_confirmation_and_stay_bound_to_original_tab() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("第一個.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        for protocol in [Protocol::Ssh, Protocol::Sftp, Protocol::Ftp] {
            let ctx = egui::Context::default();
            let mut app = test_app(&ctx);
            let (receiver, _events) = connect_test_worker(&mut app, protocol);
            let input = egui::RawInput {
                dropped_files: vec![dropped(&first), dropped(&second), dropped(&first)],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
            output.textures_delta.clear();
            let pending = app.pending_upload.as_ref().unwrap();
            assert_eq!(pending.paths.len(), 2);
            assert_eq!(pending.directory, "/uploads");
            assert!(receiver.try_recv().is_err());
            app.active = None;
            app.submit_dropped_upload();
            for path in [&first, &second] {
                match receiver.try_recv().unwrap() {
                    SessionCommand::Upload { local, remote_name } => {
                        assert_eq!(&local, path);
                        assert_eq!(remote_name, files::file_name_of(path));
                    }
                    command => panic!("Unexpected command: {command:?}"),
                }
            }
            assert!(receiver.try_recv().is_err());
            assert!(matches!(app.notice, Some((Level::Info, _))));
        }
    }

    #[test]
    fn invalid_drops_do_not_replace_tftp_form() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.tabs[0].tftp.local = "original.txt".into();
        for files in [
            vec![dropped(directory.path())],
            vec![dropped(&directory.path().join("missing.txt"))],
            vec![dropped(&first), dropped(&second)],
            vec![dropped(Path::new("relative.txt"))],
        ] {
            app.receive_files(&files);
            assert!(matches!(app.notice, Some((Level::Error, _))));
            assert_eq!(app.tabs[0].tftp.local, "original.txt");
        }
        app.active = None;
        app.receive_files(&[dropped(&first)]);
        assert!(matches!(app.notice, Some((Level::Error, _))));
        assert!(app.pending_upload.is_none());
    }

    #[test]
    fn changed_directory_and_dead_worker_never_report_success() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.txt");
        std::fs::write(&path, b"test").unwrap();
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let (receiver, _events) = connect_test_worker(&mut app, Protocol::Ftp);
        app.receive_files(&[dropped(&path)]);
        app.tabs[0].browser.cwd = "/changed".into();
        app.submit_dropped_upload();
        assert!(receiver.try_recv().is_err());
        assert!(matches!(app.notice, Some((Level::Error, _))));
        app.receive_files(&[dropped(&path)]);
        drop(receiver);
        app.submit_dropped_upload();
        assert!(matches!(app.notice, Some((Level::Error, _))));
    }

    #[test]
    fn disconnected_busy_and_modal_states_reject_drops() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.txt");
        std::fs::write(&path, b"test").unwrap();
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let (receiver, _events) = connect_test_worker(&mut app, Protocol::Sftp);
        app.tabs[0].connected = false;
        app.receive_files(&[dropped(&path)]);
        assert!(app.pending_upload.is_none());
        app.tabs[0].connected = true;
        app.tabs[0].busy = Some("Uploading".into());
        app.receive_files(&[dropped(&path)]);
        assert!(app.pending_upload.is_none());
        app.tabs[0].busy = None;
        app.open_settings();
        app.receive_files(&[dropped(&path)]);
        assert!(app.pending_upload.is_none());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn distinct_paths_with_same_remote_name_are_rejected() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = first_dir.path().join("same.txt");
        let second = second_dir.path().join("same.txt");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        assert!(local_files(&[dropped(&first), dropped(&second)], Language::English).is_err());
    }
}
