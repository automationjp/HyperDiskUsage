//! Exercise production egui widgets with pointer/text input, without OS dialogs or a GPU.
use std::time::{Duration, Instant};

use egui::{epaint::Shape, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

use super::*;

struct Screen {
    app: App,
    ctx: egui::Context,
    labels: Vec<(String, Rect)>,
}
impl Screen {
    fn new(app: App) -> Self {
        let mut screen = Self {
            app,
            ctx: egui::Context::default(),
            labels: Vec::new(),
        };
        screen.frame(Vec::new());
        // The initial egui pass measures auto-sized panels before painting their contents.
        screen.frame(Vec::new());
        screen
    }
    fn frame(&mut self, events: Vec<Event>) {
        let output = self.ctx.run(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 900.0))),
                events,
                ..Default::default()
            },
            |ctx| self.app.show_ui(ctx),
        );
        self.labels.clear();
        fn collect(shape: &Shape, labels: &mut Vec<(String, Rect)>) {
            match shape {
                Shape::Text(text) => labels.push((
                    text.galley.text().to_owned(),
                    text.galley.rect.translate(text.pos.to_vec2()),
                )),
                Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, labels);
                    }
                }
                _ => {}
            }
        }
        for shape in &output.shapes {
            collect(&shape.shape, &mut self.labels);
        }
    }
    fn position(&self, label: &str) -> Pos2 {
        self.labels
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("Missing {label:?}; rendered {:?}", self.labels))
            .1
            .center()
    }
    fn click(&mut self, label: &str) {
        let pos = self.position(label);
        self.frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        self.frame(vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
    }
    fn sees(&self, text: &str) -> bool {
        self.labels.iter().any(|(label, _)| label.contains(text))
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.app.scanning() && Instant::now() < deadline {
            self.frame(Vec::new());
            std::thread::yield_now();
        }
        assert!(
            !self.app.scanning(),
            "worker did not finish within 10 seconds"
        );
        self.frame(Vec::new());
    }
}

#[test]
fn typed_path_and_start_button_render_results_and_allow_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("payload");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("data.bin"), [0u8; 512]).unwrap();
    let mut screen = Screen::new(App::default());
    screen.click("C:\\ またはフォルダのパス");
    screen.frame(vec![Event::Text(dir.path().display().to_string())]);
    assert_eq!(screen.app.root_input, dir.path().display().to_string());
    screen.click("スキャン開始");
    assert!(screen.app.scanning());
    assert!(screen.sees("スキャン中") && screen.sees("走査済み"));
    screen.finish();
    assert!(screen.app.complete && screen.sees("完了"));
    assert_eq!(screen.app.model.total_of(dir.path()).files, 1);
    assert_eq!(screen.app.model.total_of(dir.path()).logical, 512);
    assert!(screen.sees("📁 payload"));
    screen.click("📁 payload");
    assert_eq!(screen.app.current, child);
    screen.frame(Vec::new()); // Navigation takes effect on the next immediate-mode paint.
    assert!(screen.sees("直下のファイル合計: 1 files"));
}

#[test]
fn cancel_button_cannot_turn_a_queued_finish_into_success() {
    let (tx, handle) = scan::test_handle(2);
    let mut screen = Screen::new(App {
        scan: Some(handle),
        started_at: Some(Instant::now()),
        state: "スキャン中".into(),
        ..Default::default()
    });
    screen.click("中止");
    assert!(screen.app.cancelling && !screen.app.complete);
    assert!(screen.sees("中断処理中"));
    tx.send(scan::Msg::Core(hyperdu_core::ScanEvent::Finished))
        .unwrap();
    screen.frame(Vec::new());
    assert!(!screen.app.complete && !screen.app.scanning());
    assert!(screen.sees("中断しました"));
}

#[test]
fn mode_and_sort_buttons_affect_the_next_scan_and_rendered_order() {
    let dir = tempfile::tempdir().unwrap();
    for (name, size) in [("alpha", 8), ("zulu", 512)] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
        std::fs::write(dir.path().join(name).join("data.bin"), vec![0u8; size]).unwrap();
    }
    let mut screen = Screen::new(App::default());
    screen.click("一括");
    assert_eq!(screen.app.params.mode, hyperdu_core::ScanMode::Batch);
    screen.click("C:\\ またはフォルダのパス");
    screen.frame(vec![Event::Text(dir.path().display().to_string())]);
    screen.click("スキャン開始");
    screen.finish();
    assert!(screen.app.complete);
    screen.click("論理サイズ");
    assert!(screen.position("📁 zulu").y < screen.position("📁 alpha").y);
    screen.click("名前");
    assert!(screen.position("📁 alpha").y < screen.position("📁 zulu").y);
}
