//! Real executable transport test; desktop effects are separately tested via a fake adapter.
#![cfg(not(windows))]
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
fn executable_speaks_json_lines_and_reports_unsupported_platform_honestly() {
    let dir =
        std::env::temp_dir().join(format!("remote-window-bridge-stdio-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_remote-window-bridge"))
        .args(["mcp", "--transport", "stdio", "--data-dir"])
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let mut guard = ChildGuard(child);
    let send = |child: &mut Child, value: Value| {
        let stdin = child.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    let read = || -> Value {
        serde_json::from_str(
            &rx.recv_timeout(Duration::from_secs(5))
                .expect("MCP response timeout"),
        )
        .expect("stdout must contain JSON only")
    };
    send(
        &mut guard.0,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"stdio-test","version":"1"},"capabilities":{}}}),
    );
    assert_eq!(
        read()["result"]["serverInfo"]["name"],
        "remote-window-bridge"
    );
    send(
        &mut guard.0,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    send(
        &mut guard.0,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    assert!(read()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "desktop_click"));
    send(
        &mut guard.0,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_list_windows","arguments":{}}}),
    );
    let response = read();
    assert_eq!(response["result"]["isError"], true);
    let error: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "PLATFORM_UNSUPPORTED");
    guard
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"invalid json\n")
        .unwrap();
    guard.0.stdin.as_mut().unwrap().flush().unwrap();
    assert_eq!(read()["error"]["code"], -32700);
    send(
        &mut guard.0,
        json!({"jsonrpc":"2.0","id":4,"method":"ping"}),
    );
    assert_eq!(read()["id"], 4);
    guard.0.stdin.take();
    let start = Instant::now();
    loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "Server must exit on stdin EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(dir);
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
