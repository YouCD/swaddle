use env_logger::Env;
use std::path::PathBuf;
use swaddle::{read_or_create_config, IdleApp};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // 解析 --config <路径> 命令行参数
    let config_path = match args.iter().position(|a| a == "--config") {
        Some(i) => args.get(i + 1).map(PathBuf::from),
        None => None,
    };

    let config = match config_path {
        Some(path) => read_or_create_config(Some(path)),
        None => read_or_create_config(None),
    };
    let mut app = IdleApp::new(config);
    let log_level = if app.config.debug { "debug" } else { "info" };

    env_logger::Builder::from_env(Env::default().default_filter_or(log_level)).init();

    log::debug!("Swaddle: Starting up . . .");

    let _ = app.run();
}