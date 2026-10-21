//! Launches the bundled helper without input lease, consent prompts or desktop input.
fn main() {
    #[cfg(target_os = "macos")]
    {
        let diagnostics = remote_window_bridge::platform::macos::diagnostics(None).unwrap();
        assert_eq!(diagnostics["capture_backend"], "screencapturekit_window");
        assert!(diagnostics["screen_capture"].is_boolean());
        assert!(diagnostics["input_control"].is_boolean());
        println!("Native helper launched and returned diagnostics: {diagnostics}");
    }
    #[cfg(not(target_os = "macos"))]
    println!("This check requires macOS 14+");
}
