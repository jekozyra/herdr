use super::*;

#[test]
fn native_drop_accepts_pdf_quotes_and_rejects_prose_and_remote_paths() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report with spaces.pdf");
    std::fs::write(&file, b"test pdf").unwrap();
    let text = format!("'{}' ", file.display());
    assert_eq!(dropped_paths(&text).unwrap(), vec![file]);
    assert!(dropped_paths("is this pdf available?").is_none());
    assert!(dropped_paths("/missing/remote/file.pdf").is_none());
    assert!(dropped_paths(&format!("please read {text}")).is_none());
    let image = dir.path().join("screenshot.PNG");
    std::fs::write(&image, b"test image").unwrap();
    assert!(dropped_paths(image.to_str().unwrap()).is_none());
    assert!(dropped_paths(&format!("'{}' {text}", image.display())).is_none());
    assert_eq!(
        drop_text(b"\x1b[200~/tmp/test.pdf\x1b[201~"),
        Some("/tmp/test.pdf")
    );
    assert!(drop_text(b"\x1b[200~/tmp/test.pdf").is_none());
}

#[test]
fn defaults_and_target_overrides_preserve_selected_machine() {
    let cfg: config::FileUploadConfig = toml::from_str(
        r#"
[limits]
max_file_bytes = 12
[targets.dev]
destination = "~/files"
"#,
    )
    .unwrap();
    let (target, _, limits) = cfg.resolve("dev", "work").unwrap();
    assert_eq!(target.host, "dev");
    assert_eq!(target.socket, "~/.config/herdr/sessions/work/herdr.sock");
    assert_eq!(limits.max_file_bytes, 12);
    let (target, _, limits) = config::FileUploadConfig::default()
        .resolve("dev", "work")
        .unwrap();
    assert_eq!(limits.max_file_bytes, 100 * 1024 * 1024);
    assert_eq!(target.destination, "~/.cache/herdr/uploads");
    assert_eq!(target.retention_seconds, 30 * 24 * 60 * 60);
    assert_eq!(target.socket, "~/.config/herdr/sessions/work/herdr.sock");
}

#[cfg(unix)]
mod native_flow {
    use super::*;
    use crate::client::endpoint::{EndpointNegotiation, EndpointTransport, SavedSshEndpoint};
    use std::{
        io::{BufRead, Write},
        os::unix::{fs::PermissionsExt, net::UnixListener},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
    };

    struct Sink;
    impl EndpointTransport for Sink {
        fn send(&mut self, _: &crate::protocol::ClientMessage) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        file: PathBuf,
        config: config::FileUploadConfig,
        profile: SavedSshEndpoint,
        registry: EndpointRegistry,
        calls: Arc<Mutex<Vec<serde_json::Value>>>,
        stop: Arc<AtomicBool>,
        server: Option<std::thread::JoinHandle<()>>,
    }
    impl Fixture {
        fn new(changed_terminal: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let file = root.join("report 'quoted'.pdf");
            std::fs::write(&file, vec![42; 18000]).unwrap();
            let fake = root.join("ssh");
            std::fs::write(
                &fake,
                "#!/bin/sh\nfor arg do last=$arg; done\nexec sh -c \"$last\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            let socket = root.join("api.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let requests = Arc::clone(&calls);
            let stop = Arc::new(AtomicBool::new(false));
            let shutdown = Arc::clone(&stop);
            let server = std::thread::spawn(move || {
                while !shutdown.load(Ordering::Acquire) {
                    let Ok((mut client, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    client
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut line = String::new();
                    std::io::BufReader::new(client.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let mut calls = requests.lock().unwrap();
                    calls.push(request.clone());
                    let terminal = if changed_terminal && calls.len() > 1 {
                        "replaced"
                    } else {
                        "terminal-1"
                    };
                    let result = if request["method"] == "pane.get" {
                        serde_json::json!({"pane": {"pane_id":"pane_1", "terminal_id":terminal, "agent_status":"idle", "agent":"codex"}})
                    } else {
                        serde_json::json!({"type":"ok"})
                    };
                    writeln!(
                        client,
                        "{}",
                        serde_json::json!({"id":request["id"], "result":result})
                    )
                    .unwrap();
                }
            });
            let mut config = config::FileUploadConfig::default();
            config.ssh.executable = fake.to_string_lossy().into();
            let target: config::TargetOverride = toml::from_str(&format!(
                "destination = {:?}\nsocket = {:?}",
                root.join("uploads"),
                socket
            ))
            .unwrap();
            config.targets.insert("dev".into(), target);
            let profile = SavedSshEndpoint::new("Test", "dev", "default").unwrap();
            let mut registry = EndpointRegistry::empty();
            let endpoint = ClientEndpointId::Ssh(profile.id.clone());
            registry.insert(
                endpoint.clone(),
                Sink,
                1,
                EndpointNegotiation::default(),
                true,
            );
            registry.set_active(&endpoint);
            registry.unfreeze_input();
            Self {
                dir,
                file,
                config,
                profile,
                registry,
                calls,
                stop,
                server: Some(server),
            }
        }
        fn catalog(&self) -> EndpointCatalog {
            let mut catalog = EndpointCatalog::default();
            catalog.ssh.push(self.profile.clone());
            catalog
        }
        fn payload(&self) -> String {
            ssh::shell_quote(self.file.to_str().unwrap())
        }
        async fn finish(&self, uploads: &mut Uploads, shell: &mut ClientShellState) {
            let until = Instant::now() + Duration::from_secs(8);
            while uploads.job.is_some() {
                assert!(Instant::now() < until, "native upload did not finish");
                uploads.poll(&self.registry, shell).await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn native_drop_uploads_and_inserts_into_original_pane_without_enter() {
        let mut fixture = Fixture::new(false);
        let mut uploads = Uploads::new(fixture.config.clone());
        let mut shell = ClientShellState::file_upload_test_shell();
        assert!(uploads.try_drop(
            &fixture.payload(),
            &fixture.catalog(),
            &fixture.registry,
            &shell
        ));
        // Changing selection must not send this drop to the new endpoint.
        fixture.registry.insert(
            ClientEndpointId::Local,
            Sink,
            2,
            EndpointNegotiation::default(),
            true,
        );
        fixture.registry.set_active(&ClientEndpointId::Local);
        fixture.finish(&mut uploads, &mut shell).await;
        let calls = fixture.calls.lock().unwrap();
        assert_eq!(calls.len(), 3, "{:?}", uploads.notice);
        assert_eq!(calls[2]["method"], "pane.send_input");
        assert_eq!(calls[2]["params"]["pane_id"], "pane_1");
        assert_eq!(calls[2]["params"]["keys"], serde_json::json!([]));
        let paths = files::parse_paths(calls[2]["params"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read(&paths[0]).unwrap(),
            std::fs::read(&fixture.file).unwrap()
        );
        assert!(uploads
            .notice
            .as_ref()
            .unwrap()
            .0
            .contains("Uploaded and inserted"));
    }

    #[tokio::test]
    async fn oversized_drop_is_consumed_without_remote_input_or_upload() {
        let fixture = Fixture::new(false);
        let mut config = fixture.config.clone();
        config.limits.max_file_bytes = 1;
        let mut uploads = Uploads::new(config);
        let mut shell = ClientShellState::file_upload_test_shell();
        assert!(uploads.try_drop(
            &fixture.payload(),
            &fixture.catalog(),
            &fixture.registry,
            &shell
        ));
        fixture.finish(&mut uploads, &mut shell).await;
        assert!(fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call["method"] == "pane.get"));
        assert!(uploads.notice.as_ref().unwrap().0.contains("exceeds"));
        assert!(!fixture.dir.path().join("uploads").exists());
    }

    #[tokio::test]
    async fn replaced_terminal_receives_no_path() {
        let fixture = Fixture::new(true);
        let mut uploads = Uploads::new(fixture.config.clone());
        let mut shell = ClientShellState::file_upload_test_shell();
        uploads.try_drop(
            &fixture.payload(),
            &fixture.catalog(),
            &fixture.registry,
            &shell,
        );
        fixture.finish(&mut uploads, &mut shell).await;
        assert_eq!(fixture.calls.lock().unwrap().len(), 2);
        assert!(uploads
            .notice
            .as_ref()
            .unwrap()
            .0
            .contains("terminal changed"));
    }

    #[tokio::test]
    async fn cancel_and_replaced_connection_never_insert() {
        for cancel in [true, false] {
            let mut fixture = Fixture::new(false);
            let mut uploads = Uploads::new(fixture.config.clone());
            let mut shell = ClientShellState::file_upload_test_shell();
            uploads.try_drop(
                &fixture.payload(),
                &fixture.catalog(),
                &fixture.registry,
                &shell,
            );
            if cancel {
                assert!(uploads.cancel());
            } else {
                fixture.registry.insert(
                    ClientEndpointId::Ssh(fixture.profile.id.clone()),
                    Sink,
                    2,
                    EndpointNegotiation::default(),
                    true,
                );
            }
            uploads.poll(&fixture.registry, &mut shell).await;
            assert!(uploads.job.is_none());
            assert!(fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call["method"] != "pane.send_input"));
        }
    }
}
