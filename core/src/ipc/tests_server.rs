#[cfg(test)]
mod tests {
    use crate::config::{Config, GlobalSettings, ModelConfig, PreferencesConfig};
    use crate::council::CouncilPipelineConfig;
    use crate::ipc::*;
    use crate::proxy::ProxyState;
    use std::io::BufRead;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn read_response(stream: &mut UnixStream) -> String {
        let mut reader = std::io::BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    #[test]
    fn test_handle_request_sync_valid_request() {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("test.sock");

        // Create a minimal IpcState for the handler.
        let script_path = tmp.path().join("dummy.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho ok\n").ok();
        #[cfg(unix)]
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script_path)
            .status()
            .ok();
        let config_content = format!(
            "schema_version = 1\n\n[[models]]\nid = \"test\"\nname = \"Test\"\nscript_path = \"{}\"\nport = 9999\nhealth_timeout_sec = 5\n",
            script_path.display()
        );
        let config: Config = toml::from_str(&config_content).unwrap();
        let mut state = IpcState::new(config);

        let listener = UnixListener::bind(&socket_path).unwrap();

        // Server thread: accept and handle the request.
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_request_sync(stream, &mut state);
            }
        });

        // Client: connect and send a valid status request.
        let mut client = UnixStream::connect(&socket_path).unwrap();
        use std::io::Write;
        let request = ActionRequest {
            action: "status".to_string(),
            data: None,
        };
        let body = serde_json::to_string(&request).unwrap();
        client.write_all(body.as_bytes()).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        // Close the write half to signal EOF to the server using safe Rust std::net::Shutdown.
        client.shutdown(std::net::Shutdown::Write).unwrap();

        // Read the response.
        let response_line = read_response(&mut client);
        let response: ActionResponse = serde_json::from_str(response_line.trim()).unwrap();

        assert_eq!(response.status, "ok");
        assert!(response.message.contains("No active model"));
    }

    // -- Model cycling tests --------------------------------------------------

    fn make_model_cfg(id: &str, port: u16) -> ModelConfig {
        ModelConfig {
            id: id.into(),
            name: id.to_uppercase(),
            script_path: "/tmp/x".into(),
            port,
            health_timeout_sec: 5,
            ctx_size: 65_536,
        }
    }

    fn make_test_state(models: Vec<ModelConfig>) -> IpcState {
        let config = Config {
            schema_version: 1,
            models,
            global: GlobalSettings::default(),
            preferences: PreferencesConfig::default(),
            council: CouncilPipelineConfig::default(),
        };
        IpcState::new(config)
    }

    /// Minimal ProcessGuard for unit tests — never starts or terminates anything.
    struct DummyGuard;
    impl crate::process_manager::ProcessGuard for DummyGuard {
        fn setup(
            _script: &std::path::Path,
            _port: u16,
            _log_dir: &std::path::Path,
        ) -> Result<Self, crate::process_manager::ProcessError>
        where
            Self: Sized,
        {
            Ok(DummyGuard)
        }
        fn terminate(
            &self,
            _fast_shutdown: bool,
        ) -> Result<(), crate::process_manager::ProcessError> {
            Ok(())
        }
    }

    fn make_running_model(
        _id: &str,
        _name: &str,
        _port: u16,
    ) -> crate::process_manager::RunningModel {
        crate::process_manager::RunningModel {
            id: _id.to_string(),
            guard: Box::new(DummyGuard),
            state: crate::process_manager::ModelState::Ready,
        }
    }

    fn make_state_with_running(models: Vec<ModelConfig>, running_id: &str) -> IpcState {
        let config = Config {
            schema_version: 1,
            models: models.clone(),
            global: GlobalSettings::default(),
            preferences: PreferencesConfig::default(),
            council: CouncilPipelineConfig::default(),
        };
        let mut pm = crate::process_manager::ProcessManager::new(config.clone());
        let port = models
            .iter()
            .find(|m| m.id == running_id)
            .map(|m| m.port)
            .unwrap_or(0);
        let name = models
            .iter()
            .find(|m| m.id == running_id)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        pm.set_running_model(make_running_model(running_id, &name, port));
        IpcState {
            process_manager: std::sync::Mutex::new(pm),
            proxy_state: Arc::new(Mutex::new(ProxyState::new())),
            config,
        }
    }

    #[test]
    fn test_resolve_cycle_next_no_running_wraps_to_first() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)]);
        assert_eq!(
            resolve_cycle_model_id(&state.config, None, "next"),
            Some("m1".to_string())
        );
    }

    #[test]
    fn test_resolve_cycle_prev_no_running_wraps_to_last() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)]);
        assert_eq!(
            resolve_cycle_model_id(&state.config, None, "prev"),
            Some("m2".to_string())
        );
    }

    #[test]
    fn test_resolve_cycle_next_advances_index() {
        let state = make_state_with_running(
            vec![
                make_model_cfg("m1", 8001),
                make_model_cfg("m2", 8002),
                make_model_cfg("m3", 8003),
            ],
            "m1",
        );
        let pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            resolve_cycle_model_id(&state.config, pm.get_primary_model_id(), "next"),
            Some("m2".to_string())
        );
    }

    #[test]
    fn test_resolve_cycle_prev_wraps_from_first_to_last() {
        let state = make_state_with_running(
            vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)],
            "m1",
        );
        let pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            resolve_cycle_model_id(&state.config, pm.get_primary_model_id(), "prev"),
            Some("m2".to_string())
        );
    }

    #[test]
    fn test_resolve_cycle_next_wraps_from_last_to_first() {
        let state = make_state_with_running(
            vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)],
            "m2",
        );
        let pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            resolve_cycle_model_id(&state.config, pm.get_primary_model_id(), "next"),
            Some("m1".to_string())
        );
    }

    #[test]
    fn test_resolve_literal_model_id_returns_id() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001)]);
        assert_eq!(
            resolve_cycle_model_id(&state.config, None, "m1"),
            Some("m1".to_string())
        );
    }

    #[test]
    fn test_resolve_unknown_literal_returns_none() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001)]);
        assert_eq!(
            resolve_cycle_model_id(&state.config, None, "nonexistent"),
            None
        );
    }

    #[test]
    fn test_resolve_empty_models_returns_none() {
        let state = make_test_state(vec![]);
        assert_eq!(resolve_cycle_model_id(&state.config, None, "next"), None);
        assert_eq!(resolve_cycle_model_id(&state.config, None, "prev"), None);
    }

    #[test]
    fn test_dispatch_status_no_active_model() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001)]);
        let req = ActionRequest {
            action: "status".to_string(),
            data: None,
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.message, "No active model");
    }

    #[test]
    fn test_dispatch_switch_with_next_cycling() {
        let state = make_state_with_running(
            vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)],
            "m1",
        );
        let req = ActionRequest {
            action: "switch".to_string(),
            data: Some(serde_json::json!({"model_id": "next"})),
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "ok");
        assert!(resp.message.contains("Switched to 'M2'"));
        assert_eq!(
            resolve_cycle_model_id(&state.config, Some("m1"), "next"),
            Some("m2".to_string())
        );
    }

    #[test]
    fn test_dispatch_switch_with_prev_cycling() {
        let state = make_state_with_running(
            vec![make_model_cfg("m1", 8001), make_model_cfg("m2", 8002)],
            "m2",
        );
        let req = ActionRequest {
            action: "switch".to_string(),
            data: Some(serde_json::json!({"model_id": "prev"})),
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "ok");
        assert!(resp.message.contains("Switched to 'M1'"));
        assert_eq!(
            resolve_cycle_model_id(&state.config, Some("m2"), "prev"),
            Some("m1".to_string())
        );
    }

    #[test]
    fn test_dispatch_unknown_action_returns_error() {
        let state = make_test_state(vec![]);
        let req = ActionRequest {
            action: "foobar".to_string(),
            data: None,
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "error");
        assert!(resp.message.contains("unknown action"));
    }

    #[test]
    fn test_dispatch_switch_missing_model_id_returns_error() {
        let state = make_test_state(vec![]);
        let req = ActionRequest {
            action: "switch".to_string(),
            data: Some(serde_json::json!({})),
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "error");
        assert!(resp.message.contains("missing model_id"));
    }

    #[test]
    fn test_dispatch_switch_nonexistent_model_returns_error() {
        let state = make_test_state(vec![]);
        let req = ActionRequest {
            action: "switch".to_string(),
            data: Some(serde_json::json!({"model_id": "ghost"})),
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "error");
        assert!(resp.message.contains("not found"));
    }

    #[test]
    fn test_dispatch_list_returns_models() {
        let state = make_test_state(vec![make_model_cfg("m1", 8001)]);
        let req = ActionRequest {
            action: "list".to_string(),
            data: None,
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "ok");
        let data = resp.data.unwrap();
        let models = data["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"].as_str().unwrap(), "m1");
    }

    #[test]
    fn test_dispatch_stop_clears_proxy() {
        let state = make_test_state(vec![]);
        {
            let mut ps = state.proxy_state.lock().unwrap_or_else(|e| e.into_inner());
            ps.set_target(8001);
        }
        let req = ActionRequest {
            action: "stop".to_string(),
            data: None,
        };
        let mut pm = state
            .process_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let resp = dispatch_action(req, &mut pm, &state);
        assert_eq!(resp.status, "ok");
        let ps = state.proxy_state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(ps.primary_port.is_none());
        assert!(ps.active_models.is_empty());
    }
}
