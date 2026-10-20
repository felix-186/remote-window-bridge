use crate::{
    error::{DesktopError, Result},
    geometry::Rect,
    types::{WindowIdentity, WindowInfo},
};

/// AweSun exposes an outer Flutter window and a separate top-level view window.
/// Never infer equivalence from PID alone, or from an owner shared by sessions.
pub(super) fn awesun_counterpart(
    target: &WindowInfo,
    client: Rect,
    candidates: &[(WindowInfo, Rect)],
) -> Result<Option<WindowIdentity>> {
    let Some(expected) = super::clients::awesun_expected_class(target) else {
        return Ok(None);
    };
    let mut matches = candidates.iter().filter(|(candidate, rect)| {
        candidate.identity.id != target.identity.id
            && candidate.identity.class_name == expected
            && candidate.identity.process_id == target.identity.process_id
            && candidate.identity.process_started == target.identity.process_started
            && candidate
                .executable
                .eq_ignore_ascii_case(&target.executable)
            && candidate.title == target.title
            && *rect == client
    });
    let first = matches.next();
    let duplicate_target = candidates.iter().any(|(candidate, rect)| {
        candidate.identity.id != target.identity.id
            && candidate.identity.class_name == target.identity.class_name
            && candidate.identity.process_id == target.identity.process_id
            && candidate.identity.process_started == target.identity.process_started
            && candidate
                .executable
                .eq_ignore_ascii_case(&target.executable)
            && candidate.title == target.title
            && *rect == client
    });
    if matches.next().is_some() || (first.is_some() && duplicate_target) {
        return Err(DesktopError::new(
            "WINDOW_AMBIGUOUS",
            "Multiple matching AweSun surfaces; select an unambiguous remote session",
        ));
    }
    Ok(first.map(|(window, _)| window.identity.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(id: &str, class: &str) -> WindowInfo {
        WindowInfo {
            identity: WindowIdentity {
                id: crate::types::WindowId(id.into()),
                process_id: 7,
                process_started: 10,
                class_name: class.into(),
            },
            title: "ZhangQ".into(),
            executable: "C:\\AweSun.exe".into(),
        }
    }
    const CLIENT: Rect = Rect {
        x: 82,
        y: 109,
        width: 1910,
        height: 1120,
    };
    #[test]
    fn pairs_only_the_unique_view_of_the_selected_session() {
        let target = info("outer", "FlutterMultiWindow");
        let view = info("view", "ORAY_FLUTTER_VIEW_CONTAINER");
        let mut other = info("other-session", "ORAY_FLUTTER_VIEW_CONTAINER");
        other.title = "Another remote host".into();
        let found = awesun_counterpart(&target, CLIENT, &[(other, CLIENT), (view, CLIENT)])
            .unwrap()
            .unwrap();
        assert_eq!(found.id.0, "view");
    }
    #[test]
    fn pairing_is_unambiguous_in_both_directions() {
        let target = info("view", "ORAY_FLUTTER_VIEW_CONTAINER");
        let outer = info("outer", "FlutterMultiWindow");
        let candidates = [
            (outer, CLIENT),
            (info("second-view", "ORAY_FLUTTER_VIEW_CONTAINER"), CLIENT),
        ];
        assert_eq!(
            awesun_counterpart(&target, CLIENT, &candidates)
                .unwrap_err()
                .code,
            "WINDOW_AMBIGUOUS"
        );
        let foreign = WindowInfo {
            executable: "C:\\Another.exe".into(),
            ..info("outer", "FlutterMultiWindow")
        };
        assert!(awesun_counterpart(&foreign, CLIENT, &candidates)
            .unwrap()
            .is_none());
    }

    #[test]
    fn rejects_ambiguous_or_unrelated_windows_even_in_the_same_process() {
        let target = info("outer", "FlutterMultiWindow");
        for change in 0..6 {
            let mut view = info("view", "ORAY_FLUTTER_VIEW_CONTAINER");
            let mut rect = CLIENT;
            match change {
                0 => view.identity.process_id += 1,
                1 => view.identity.process_started += 1,
                2 => view.title = "Different".into(),
                3 => rect.x += 1,
                4 => view.executable = "C:\\Another.exe".into(),
                _ => view.identity.class_name = "FLUTTER_RUNNER_WIN32_WINDOW".into(),
            }
            assert!(awesun_counterpart(&target, CLIENT, &[(view, rect)])
                .unwrap()
                .is_none());
        }
        let candidates = [
            (info("v1", "ORAY_FLUTTER_VIEW_CONTAINER"), CLIENT),
            (info("v2", "ORAY_FLUTTER_VIEW_CONTAINER"), CLIENT),
        ];
        assert_eq!(
            awesun_counterpart(&target, CLIENT, &candidates)
                .unwrap_err()
                .code,
            "WINDOW_AMBIGUOUS"
        );
    }
}
