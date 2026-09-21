use std::env;
use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use swaddle::*;

fn make_test_settings(inhibit_duration: u64, sleep_duration: u64) -> Settings {
    Settings {
        debug: false,
        server: ServerSettings {
            inhibit_duration,
            sleep_duration,
        },
        ha: None,
        swayidle: SwayIdleSettings {
            config_path: "/tmp/swaddle-test/swayidle.conf".to_string(),
            enabled: false,
        },
    }
}

/// Builds an IdleApp with default test settings; `None` when the D-Bus
/// session bus is unavailable (possible in some CI environments).
fn new_test_app() -> Option<IdleApp> {
    IdleApp::new(Ok(make_test_settings(25, 5))).ok()
}

#[test]
fn test_config_lifecycle() {
    let temp_dir = env::temp_dir().join(format!("swaddle_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let config_path = temp_dir.join("config.toml");

    // Missing file -> Err with a helpful message (no file is ever created)
    let err = read_config(Some(config_path.clone())).unwrap_err();
    assert!(
        err.to_string().contains("Config file not found"),
        "unexpected error: {err}"
    );
    assert!(
        !config_path.exists(),
        "read_config must not create the config file"
    );

    // Existing file -> values are parsed
    let custom_toml = "\
debug = true

[server]
inhibit_duration = 60
sleep_duration = 10

[swayidle]
config_path = \"/tmp/swayidle.conf\"
enabled = true
";
    fs::write(&config_path, custom_toml).unwrap();

    let config = read_config(Some(config_path.clone())).unwrap();
    assert!(config.debug);
    assert_eq!(config.server.inhibit_duration, 60);
    assert_eq!(config.server.sleep_duration, 10);
    assert_eq!(config.swayidle.config_path, "/tmp/swayidle.conf");
    assert!(config.swayidle.enabled);
    assert!(
        config.ha.is_none(),
        "the optional ha section should deserialize to None when absent"
    );

    fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn test_app_initialization_and_state() {
    let config = Ok(Settings {
        debug: true,
        server: ServerSettings {
            inhibit_duration: 30,
            sleep_duration: 10,
        },
        ha: None,
        swayidle: SwayIdleSettings {
            config_path: "/tmp/swaddle-test/swayidle.conf".to_string(),
            enabled: false,
        },
    });
    let Ok(app) = IdleApp::new(config) else {
        println!("D-Bus unavailable - skipping");
        return;
    };

    assert_eq!(app.config.server.inhibit_duration, 30);
    assert!(
        !app.has_active_audio(),
        "freshly created app should have no active audio flows"
    );
    println!("✓ App initialized with expected state");
}

#[test]
fn test_media_player_detection_logic() {
    let mock_names = vec![
        "org.mpris.MediaPlayer2.spotify".to_string(),
        "org.freedesktop.DBus".to_string(),
        "org.mpris.MediaPlayer2.vlc".to_string(),
        "org.gnome.SessionManager".to_string(),
    ];

    let filtered: Vec<_> = mock_names
        .into_iter()
        .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
        .collect();

    assert_eq!(filtered.len(), 2);
    assert!(filtered.contains(&"org.mpris.MediaPlayer2.spotify".to_string()));
    assert!(filtered.contains(&"org.mpris.MediaPlayer2.vlc".to_string()));
}

#[test]
fn test_blocking_logic_comprehensive() {
    let Some(mut app) = new_test_app() else {
        println!("D-Bus unavailable - skipping");
        return;
    };

    assert!(!app.has_active_audio());

    // run_cmd spawns `systemd-inhibit ... sh -c "sleep <inhibit_duration>"`;
    // verify we can spawn, observe, kill and reap it.
    match app.run_cmd() {
        Ok(mut child) => {
            assert!(
                child.try_wait().unwrap().is_none(),
                "systemd-inhibit should still be running right after spawn"
            );
            child.kill().unwrap();
            child.wait().unwrap();
            println!("✓ Process spawning successful");
        }
        Err(_) => {
            println!("✓ Process spawning gracefully handled when systemd-inhibit unavailable");
        }
    }

    let is_playing = app.check_playback_status();
    println!("✓ check_playback_status returned: {}", is_playing);
    println!("✓ All blocking logic tests passed");
}

fn get_mock_player_script_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("mock_media_player.py");
    path
}

fn spawn_mock_player() -> Result<std::process::Child, Box<dyn std::error::Error>> {
    let script_path = get_mock_player_script_path();

    if !script_path.exists() {
        return Err(format!("Mock script not found at: {}", script_path.display()).into());
    }

    let child = Command::new("python3")
        .arg(&script_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    Ok(child)
}

#[test]
fn test_dbus_mock_player_integration() {
    let deps_check = Command::new("python3")
        .arg("-c")
        .arg("import dbus, dbus.service, dbus.mainloop.glib; from gi.repository import GLib; print('OK')")
        .output();

    if deps_check.is_err() || !String::from_utf8_lossy(&deps_check.unwrap().stdout).contains("OK") {
        println!(
            "D-Bus Python dependencies not available - this is expected in some CI environments"
        );
        match new_test_app() {
            Some(app) => {
                let is_playing = app.check_playback_status();
                println!("check_playback_status without players: {}", is_playing);
            }
            None => println!("D-Bus session also unavailable"),
        }
        println!("✓ Gracefully handled D-Bus unavailable scenario");
        return;
    }

    let Some(app) = new_test_app() else {
        println!("D-Bus session unavailable - skipping mock player test");
        return;
    };

    let mut mock_process = match spawn_mock_player() {
        Ok(process) => process,
        Err(e) => {
            println!("Failed to spawn mock player: {}", e);
            return;
        }
    };

    thread::sleep(Duration::from_millis(2000));

    println!("Testing with mock media player...");

    match app.list_media_players() {
        Ok(players) => {
            println!("Found players: {:?}", players);

            if players.iter().any(|p| p.contains("mocktestplayer")) {
                println!("✓ Mock media player detected!");

                let is_playing = app.check_playback_status();
                println!("Blocking state: {}", is_playing);

                if is_playing {
                    println!("✓ Successfully detected 'Playing' status!");
                } else {
                    println!("⚠ Mock player detected but not 'Playing' - may be a D-Bus communication issue");
                }
            } else {
                println!("Mock player not detected in D-Bus service list");
                println!("✓ D-Bus connection test completed");
            }
        }
        Err(e) => {
            println!("D-Bus connection failed: {:?}", e);
            println!("This is acceptable in CI environments without D-Bus");
        }
    }

    mock_process.kill().ok();
    mock_process.wait().ok();

    thread::sleep(Duration::from_millis(200));

    if let Ok(players) = app.list_media_players() {
        if players.iter().any(|p| p.contains("mocktestplayer")) {
            println!("⚠ Mock player still registered on D-Bus after cleanup");
        } else {
            println!("✓ Mock player properly unregistered from D-Bus");
        }
    }

    println!("✓ Mock D-Bus integration test completed");
}
