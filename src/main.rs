use airemotecontrol::{
    control::Control,
    error::{DesktopError, Result},
    geometry::Rect,
    mcp, platform,
    service::DesktopService,
    types::WindowId,
};
use std::{path::PathBuf, sync::Arc};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        println!("AIRemoteControl {}\n\nCommands:\n  doctor\n  windows\n  capture --window <opaque-id> [--viewport x,y,width,height] [--output preview.png] [--delay-ms 3000]\n  mcp [--transport stdio] [--data-dir <directory>]\n\nWindows host only. Connect the remote client first.\nCtrl+Alt+F10: pause   Ctrl+Alt+F11: resume locally\nNo OCR, background capture, or GUI in v0.1. See README.md.",env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut options = std::collections::HashMap::new();
    while let Some(key) = args.next() {
        if !key.starts_with("--") {
            return Err(DesktopError::invalid(format!("Unexpected argument {key}")));
        }
        let value = args
            .next()
            .ok_or_else(|| DesktopError::invalid(format!("Missing value for {key}")))?;
        if options.insert(key.clone(), value).is_some() {
            return Err(DesktopError::invalid(format!("Duplicate option {key}")));
        }
    }
    let allowed: &[&str] = match command.as_str() {
        "doctor" | "windows" => &[],
        "capture" => &[
            "--window",
            "--viewport",
            "--output",
            "--delay-ms",
            "--data-dir",
        ],
        "mcp" => &["--transport", "--data-dir"],
        _ => return Err(DesktopError::invalid("Unknown command; run --help")),
    };
    if let Some(key) = options.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(DesktopError::invalid(format!("Unknown option {key}")));
    }
    let control = Arc::new(Control::default());
    let native = platform::native(control.clone())?;
    if command == "doctor" {
        println!(
            "{}",
            serde_json::to_string_pretty(&native.capabilities()).unwrap()
        );
        return Ok(());
    }
    if command == "windows" {
        println!(
            "{}",
            serde_json::to_string_pretty(&native.list_windows()?).unwrap()
        );
        return Ok(());
    }
    let data_dir = options
        .get("--data-dir")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("AIREMOTE_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|dir| PathBuf::from(dir).join("AIRemoteControl").join("data"))
                .unwrap_or_else(|| PathBuf::from("data"))
        });
    let mut service = DesktopService::new(native, control, data_dir)?;
    if command == "mcp" {
        if options
            .get("--transport")
            .is_some_and(|value| value != "stdio")
        {
            return Err(DesktopError::invalid("Only stdio transport is implemented"));
        }
        return mcp::serve(service)
            .map_err(|e| DesktopError::new("TRANSPORT_ERROR", e.to_string()));
    }
    let window_id = options.get("--window").ok_or_else(|| {
        DesktopError::invalid("--window required; get an id from the windows command")
    })?;
    let viewport = options
        .get("--viewport")
        .map(|s| parse_viewport(s))
        .transpose()?;
    service.attach(WindowId(window_id.clone()), viewport)?;
    let delay = options
        .get("--delay-ms")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| DesktopError::invalid("Invalid delay"))
        })
        .transpose()?
        .unwrap_or(3000);
    if delay > 60000 {
        return Err(DesktopError::invalid("delay-ms must be <= 60000"));
    }
    eprintln!("Select the remote window now. Capturing in {delay}ms.");
    std::thread::sleep(std::time::Duration::from_millis(delay));
    let metadata = service.observe()?;
    let output = options
        .get("--output")
        .map(String::as_str)
        .unwrap_or("preview.png");
    std::fs::write(output, &service.frame.as_ref().unwrap().png)
        .map_err(|e| DesktopError::new("STORAGE_UNAVAILABLE", e.to_string()))?;
    println!("{}", serde_json::json!({"output":output,"frame":metadata}));
    Ok(())
}
fn parse_viewport(value: &str) -> Result<Rect> {
    let fields: Vec<i64> = value
        .split(',')
        .map(|s| {
            s.parse::<i64>()
                .map_err(|_| DesktopError::invalid("viewport must be x,y,width,height"))
        })
        .collect::<Result<_>>()?;
    if fields.len() != 4 || fields.iter().any(|v| *v < 0 || *v > 32768) {
        return Err(DesktopError::invalid("Invalid viewport"));
    }
    Rect {
        x: fields[0] as i32,
        y: fields[1] as i32,
        width: fields[2] as u32,
        height: fields[3] as u32,
    }
    .validate()
}
