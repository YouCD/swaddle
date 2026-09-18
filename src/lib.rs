use config::{Config, File};
use dbus::{
    arg::messageitem::MessageItem,
    blocking::{BlockingSender, Connection},
    Message,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    error::Error,
    fs::{self, create_dir_all},
    path::PathBuf,
    process::{Child, Command},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Settings {
    pub debug: bool,
    pub server: ServerSettings,
    #[serde(default)]
    pub ha: Option<HaSettings>,
    pub swayidle: SwayIdleSettings,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ServerSettings {
    pub inhibit_duration: u64,
    pub sleep_duration: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HaSettings {
    pub host: String,
    pub token: String,
    pub entity: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SwayIdleSettings {
    pub config_path: String,
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            debug: false,
            server: ServerSettings {
                inhibit_duration: 25,
                sleep_duration: 5,
            },
            ha: Some(HaSettings {
                host: "http://192.168.1.188:8123".to_string(),
                token: String::new(),
                entity: "switch.cuco_cp5d_306c_switch_2".to_string(),
                enabled: true,
            }),
            swayidle: SwayIdleSettings {
                config_path: format!(
                    "{}/.config/niri/swayidle.conf",
                    std::env::var("HOME").unwrap_or_default()
                ),
                enabled: true,
            },
        }
    }
}

/// 共享状态：PipeWire 检测到的活跃音频流集合 + 是否正在播放
#[derive(Debug, Default)]
struct PipeWireState {
    active_flows: HashSet<u32>,
}

impl PipeWireState {
    fn has_active_audio(&self) -> bool {
        !self.active_flows.is_empty()
    }
}

/// 锁屏抑制状态
#[derive(Debug, Default)]
struct InhibitState {
    swayidle_running: bool,
    child: Option<Child>,
}

pub struct IdleApp {
    pub conn: Connection,
    pub config: Settings,
    pipewire_state: Arc<Mutex<PipeWireState>>,
    inhibit_state: Arc<Mutex<InhibitState>>,
}

impl IdleApp {
    pub fn new(config_from_file: Result<Settings, Box<dyn std::error::Error>>) -> IdleApp {
        let conn = Connection::new_session().expect("Failed to connect to D-Bus");
        let config = config_from_file
            .inspect_err(|_| log::debug!("No config found or parsed. Using the defaults"))
            .unwrap_or_default();
        IdleApp {
            conn,
            config,
            pipewire_state: Arc::new(Mutex::new(PipeWireState::default())),
            inhibit_state: Arc::new(Mutex::new(InhibitState::default())),
        }
    }

    // We want to check every single media player to see if they are playing
    pub fn list_media_players(&self) -> Result<Vec<String>, dbus::Error> {
        let msg = Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
        )
        .map_err(|e| dbus::Error::new_failed(&e.to_string()))?;

        let response = self
            .conn
            .send_with_reply_and_block(msg, Duration::from_secs(5))?;
        let names: Vec<String> = response
            .get1()
            .ok_or_else(|| dbus::Error::new_failed("Failed to get names from response"))?;

        Ok(names
            .into_iter()
            .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
            .collect())
    }

    pub fn check_playback_status(&self) -> bool {
        let players = match self.list_media_players() {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to list media players: {:?}", e);
                return false;
            }
        };

        log::debug!("Listing players: {:?}", players);

        for service in players {
            let object_path = "/org/mpris/MediaPlayer2";
            let interface = "org.mpris.MediaPlayer2.Player";
            let property = "PlaybackStatus";

            let msg = match Message::new_method_call(
                service,
                object_path,
                "org.freedesktop.DBus.Properties",
                "Get",
            ) {
                Ok(m) => m.append1(interface).append1(property),
                Err(e) => {
                    log::error!("Failed to create D-Bus message: {:?}", e);
                    continue;
                }
            };

            let response = self
                .conn
                .send_with_reply_and_block(msg, Duration::from_secs(5));

            log::debug!("Connection Message: {:?}", response);
            match response {
                Ok(resp) => {
                    let items = resp.get_items();
                    let Some(arg) = items.first() else {
                        log::debug!("No arguments found in the message.");
                        continue;
                    };

                    let MessageItem::Variant(ref value) = arg else {
                        log::debug!("IDK what to do...throwing away {:?}", arg);
                        continue;
                    };

                    let MessageItem::Str(ref s) = **value else {
                        log::debug!(
                            "No string inside the variant. . . . throwing away {:?}",
                            value
                        );
                        continue;
                    };

                    if s == "Playing" {
                        return true;
                    }
                }
                Err(_) => {
                    log::error!("Failed to lookup playback . . . skipping");
                }
            }
        }
        false
    }

    /// 启动 PipeWire 事件驱动监听线程。
    /// 监听音频输出流节点的活跃状态，更新 pipewire_state。
    pub fn start_pipewire_monitor(&self) {
        let state = Arc::clone(&self.pipewire_state);
        let debug = self.config.debug;
        thread::spawn(move || {
            if let Err(e) = run_pipewire_monitor(state, debug) {
                log::error!("PipeWire monitor error: {}", e);
            }
        });
    }

    /// 检查当前是否有活跃音频流（PipeWire 事件更新的共享状态）
    pub fn has_active_audio(&self) -> bool {
        self.pipewire_state.lock().unwrap().has_active_audio()
    }

    /// 检查是否应抑制锁屏（PipeWire 有声 或 MPRIS 播放中）
    pub fn should_inhibit(&self) -> bool {
        if self.has_active_audio() {
            log::debug!("Inhibiting due to active audio flow");
            return true;
        }
        if self.check_playback_status() {
            log::debug!("Inhibiting due to MPRIS playback");
            return true;
        }
        false
    }

    /// 启动或停止 swayidle 进程
    fn set_swayidle(&mut self, running: bool) {
        if !self.config.swayidle.enabled {
            return;
        }
        let mut state = self.inhibit_state.lock().unwrap();
        if state.swayidle_running == running {
            return;
        }
        if running {
            // 启动 swayidle 并持有其句柄
            if state.child.is_none() {
                log::info!("Starting swayidle");
                match Command::new("swayidle")
                    .arg("-w")
                    .arg("-C")
                    .arg(&self.config.swayidle.config_path)
                    .spawn()
                {
                    Ok(child) => {
                        state.child = Some(child);
                        state.swayidle_running = true;
                    }
                    Err(e) => log::error!("Failed to start swayidle: {}", e),
                }
            } else {
                state.swayidle_running = true;
            }
        } else {
            // 停止 swayidle 并收割，避免僵尸进程
            if let Some(mut child) = state.child.take() {
                log::info!("Stopping swayidle (media is playing)");
                let _ = child.kill();
                let _ = child.wait();
            }
            state.swayidle_running = false;
        }
    }

    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        // 启动 PipeWire 事件驱动监听
        self.start_pipewire_monitor();

        // 接管：清理可能残留的外部 swayidle，保证 swaddle 是唯一管理者
        if self.config.swayidle.enabled {
            let _ = Command::new("pkill").arg("-x").arg("swayidle").status();
            {
                let mut state = self.inhibit_state.lock().unwrap();
                state.child = None;
                state.swayidle_running = false;
            }
        }

        // 音响去抖状态
        let mut speaker_on = false;
        let mut speaker_off_started: Option<std::time::Instant> = None;

        log::info!("Swaddle starting: monitoring PipeWire and MPRIS");

        loop {
            let has_audio = self.has_active_audio();
            let mpris_playing = self.check_playback_status();
            let should_inhibit = has_audio || mpris_playing;

            log::debug!(
                "has_audio: {}, mpris_playing: {}, should_inhibit: {}",
                has_audio,
                mpris_playing,
                should_inhibit
            );

            // 1. 音响控制（基于 PipeWire 活跃音频流，仅当配置了 HA 时）
            if let Some(ha) = &self.config.ha {
                if ha.enabled {
                    if has_audio {
                        if !speaker_on {
                            log::info!("Audio active, turning speaker ON");
                            ha_speaker_call(ha, "switch/turn_on");
                            speaker_on = true;
                            speaker_off_started = None;
                        }
                    } else if speaker_on {
                        match speaker_off_started {
                            None => {
                                speaker_off_started = Some(std::time::Instant::now());
                                log::info!(
                                    "Audio stopped, will turn speaker off after {}s",
                                    self.config.server.inhibit_duration
                                );
                            }
                            Some(t) => {
                                if t.elapsed() >= Duration::from_secs(self.config.server.inhibit_duration)
                                {
                                    log::info!("Turning speaker OFF");
                                    ha_speaker_call(ha, "switch/turn_off");
                                    speaker_on = false;
                                    speaker_off_started = None;
                                }
                            }
                        }
                    }
                }
            }

            // 2. 锁屏抑制（PipeWire + MPRIS 双触发）
            self.set_swayidle(!should_inhibit);

            std::thread::sleep(Duration::from_secs(self.config.server.sleep_duration));
        }
    }

    pub fn run_cmd(&mut self) -> Result<Child, Box<dyn Error>> {
        Command::new("systemd-inhibit")
            .arg("--what")
            .arg("idle")
            .arg("--who")
            .arg("swayidle-inhibit")
            .arg("--why")
            .arg("audio playing")
            .arg("--mode")
            .arg("block")
            .arg("sh")
            .arg("-c")
            .arg(format!("sleep {}", self.config.server.inhibit_duration))
            .spawn()
            .inspect(|_| log::debug!("systemd-inhibit has been spawned"))
            .map_err(|e| {
                log::error!("Failed to execute systemd-inhibit command: {:?}", e);
                Box::from(std::io::Error::other(
                    "Unable to block swayidle due to unknown error",
                ))
            })
    }
}

/// 调用 Home Assistant 音响控制接口
fn ha_speaker_call(ha: &HaSettings, service: &str) {
    use std::time::Duration as StdDuration;

    let url = format!("{}/api/services/{}", ha.host.trim_end_matches('/'), service);
    let body = serde_json::json!({ "entity_id": ha.entity });

    let client = match reqwest::blocking::Client::builder()
        .timeout(StdDuration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to build HTTP client: {}", e);
            return;
        }
    };

    let result = client
        .post(&url)
        .bearer_auth(&ha.token)
        .json(&body)
        .send();

    match result {
        Ok(resp) => {
            if resp.status().is_success() {
                log::info!("HA {} ok", service);
            } else {
                log::error!("HA {} returned status {}", service, resp.status());
            }
        }
        Err(e) => log::error!("HA call failed ({}): {}", service, e),
    }
}

/// PipeWire 事件驱动监听：检测活跃音频输出流。
/// 通过 libpipewire registry 监听 Node 对象，跟踪 media.class=Stream/Output/Audio 的活跃状态。
fn run_pipewire_monitor(
    state: Arc<Mutex<PipeWireState>>,
    _debug: bool,
) -> Result<(), Box<dyn Error>> {
    use pipewire as pw;
    use pw::node::Node;
    use pw::proxy::{Listener, ProxyT};
    use pw::types::ObjectType;
    use std::cell::RefCell;
    use std::rc::Rc;

    pw::init();

    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;

    let registry = core.get_registry_rc()?;

    // 持有所有注册的 proxy 和 listener，确保它们在 main_loop 生命周期内存活
    let proxies: Rc<RefCell<Vec<(Box<dyn ProxyT>, Box<dyn Listener>)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let proxies_clone = Rc::clone(&proxies);
    let registry_weak = registry.downgrade();

    let _registry_listener = {
        let state_global = Arc::clone(&state);
        registry
            .add_listener_local()
            .global(move |obj| {
                if obj.type_ != ObjectType::Node {
                    return;
                }
                let is_audio_flow = obj
                    .props
                    .as_ref()
                    .and_then(|p| p.get("media.class"))
                    .map(|v| v == "Stream/Output/Audio")
                    .unwrap_or(false);
                if !is_audio_flow {
                    return;
                }

                let node_id = obj.id;
                log::debug!("Audio flow node appeared: id={}", node_id);

                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                match registry.bind::<Node, _>(obj) {
                    Ok(node) => {
                        let st = Arc::clone(&state_global);
                        let listener = node
                            .add_listener_local()
                            .info(move |info| {
                                let id = info.id();
                                let is_running =
                                    matches!(info.state(), pw::node::NodeState::Running);
                                let is_idle = matches!(info.state(), pw::node::NodeState::Idle);

                                log::debug!(
                                    "Audio flow info: id={} running={} idle={}",
                                    id,
                                    is_running,
                                    is_idle
                                );

                                let mut st = st.lock().unwrap();
                                if is_running || is_idle {
                                    st.active_flows.insert(id);
                                } else {
                                    st.active_flows.remove(&id);
                                }
                            })
                            .register();
                        // 同时保存 node proxy 和 listener，保证它们存活
                        proxies_clone.borrow_mut().push((Box::new(node), Box::new(listener)));
                    }
                    Err(e) => {
                        log::error!("Failed to bind audio flow node {}: {}", node_id, e);
                    }
                }
            })
            .global_remove(move |id| {
                log::debug!("Global removed: id={}", id);
                let mut st = state.lock().unwrap();
                st.active_flows.remove(&id);
            })
            .register()
    };

    log::info!("PipeWire monitor running");
    main_loop.run();
    log::info!("PipeWire monitor exited");

    let _ = _registry_listener;
    let _ = proxies;
    unsafe {
        pw::deinit();
    }
    Ok(())
}

fn get_config_path() -> PathBuf {
    let mut path = dirs::home_dir().expect("Could not find home directory");
    path.push(".config/swaddle/config.toml");
    path
}

pub fn read_or_create_config(custom_path: Option<PathBuf>) -> Result<Settings, Box<dyn std::error::Error>> {
    let config_path = custom_path.unwrap_or_else(get_config_path);

    if !config_path.exists() {
        let default_settings = Settings::default();
        let config_dir = config_path.parent().unwrap();
        create_dir_all(config_dir)?;
        let _ = fs::write(
            &config_path,
            toml::to_string_pretty(&default_settings).unwrap(),
        );
        return Ok(default_settings);
    }

    Ok(Config::builder()
        .add_source(File::from(config_path))
        .build()?
        .try_deserialize()?)
}