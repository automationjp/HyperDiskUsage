//! Platform font fallback.
//!
//! egui ships a Latin-only default face, so a Japanese path renders as boxes
//! until a CJK face is registered. This walks the platform's font directories
//! once at startup and installs whatever it finds as a fallback chain.
//! Moved out of the UI module unchanged: it is startup configuration, not
//! rendering, and it was half the file.

use egui::{FontData, FontDefinitions, FontFamily};

pub fn configure_fonts(ctx: &egui::Context) {
    // Start from egui defaults and add UTF-8 capable system fallbacks (CJK, Emoji).
    let mut fonts = FontDefinitions::default();

    // Helper to add a font file if present
    let mut add_font_file = |key: &str, path: &std::path::Path| -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(key.to_string(), FontData::from_owned(bytes).into());
                true
            }
            Err(_) => false,
        }
    };

    // Collect candidate font files per platform
    let (dirs, cjk_candidates, emoji_candidates, ui_candidates, mono_candidates) =
        platform_font_candidates();

    // Find first matches
    let find_first = |names: &[&str]| find_font_in_dirs(&dirs, names);

    if let Some(p) = find_first(&cjk_candidates) {
        if add_font_file("cjk", &p) {
            // Append CJK fallback
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("cjk".to_string());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("cjk".to_string());
        }
    }
    if let Some(p) = find_first(&emoji_candidates) {
        if add_font_file("emoji", &p) {
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("emoji".to_string());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("emoji".to_string());
        }
    }
    if let Some(p) = find_first(&ui_candidates) {
        if add_font_file("ui", &p) {
            // Prefer UI font first for proportional
            let fam = fonts.families.entry(FontFamily::Proportional).or_default();
            fam.insert(0, "ui".to_string());
        }
    }
    if let Some(p) = find_first(&mono_candidates) {
        if add_font_file("mono", &p) {
            let fam = fonts.families.entry(FontFamily::Monospace).or_default();
            fam.insert(0, "mono".to_string());
            // Also add as fallback to proportional for code snippets
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("mono".to_string());
        }
    }

    ctx.set_fonts(fonts);
}

/// Directories to search, then file-name candidates for each script group:
/// CJK, emoji, symbols, and a Latin fallback.
type FontCandidates = (
    Vec<std::path::PathBuf>,
    Vec<&'static str>,
    Vec<&'static str>,
    Vec<&'static str>,
    Vec<&'static str>,
);

fn platform_font_candidates() -> FontCandidates {
    #[cfg(target_os = "windows")]
    {
        let dirs = vec![std::path::PathBuf::from(r"C:\\Windows\\Fonts")];
        let cjk = vec![
            "YuGothR.ttc",
            "YuGothM.ttc",
            "meiryo.ttc",
            "MS Gothic.ttf", // JP
            "msyh.ttc",
            "msyh.ttf",
            "Microsoft YaHei.ttf",
            "SimSun.ttc", // SC
            "MingLiU.ttf",
            "PMingLiU.ttf", // TC
            "malgun.ttf",
            "Malgun Gothic.ttf", // KR
        ];
        let emoji = vec!["seguiemj.ttf", "SegoeUIEmoji.ttf"]; // Windows emoji
        let ui = vec!["segoeui.ttf", "YuGothUI.ttc", "meiryo.ttc"];
        let mono = vec![
            "consola.ttf",
            "CascadiaMono.ttf",
            "CascadiaCode.ttf",
            "msmincho.ttc",
        ];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut dirs = vec![
            std::path::PathBuf::from("/System/Library/Fonts"),
            std::path::PathBuf::from("/Library/Fonts"),
        ];
        if let Some(h) = home {
            dirs.push(h.join("Library/Fonts"));
        }
        let cjk = vec![
            "HiraginoSans-W3.ttc",
            "HiraginoSans-W4.ttc", // JP
            "PingFang.ttc",
            "PingFangSC.ttc",
            "PingFangTC.ttc",       // CN/TW
            "AppleSDGothicNeo.ttc", // KR
        ];
        let emoji = vec!["Apple Color Emoji.ttc", "AppleColorEmoji.ttf"];
        let ui = vec![
            "SFNS.ttf",
            "HelveticaNeueDeskInterface.ttc",
            "HiraginoSans-W3.ttc",
        ];
        let mono = vec!["Menlo.ttc", "SFMono.ttf", "OsakaMono.ttf"];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[cfg(target_os = "linux")]
    {
        let mut dirs = vec![
            std::path::PathBuf::from("/usr/share/fonts"),
            std::path::PathBuf::from("/usr/local/share/fonts"),
        ];
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
        let cjk = vec![
            // Noto CJK families
            "NotoSansCJK-Regular.ttc",
            "NotoSansCJKjp-Regular.otf",
            "NotoSansJP-Regular.otf",
            "NotoSansJP-Regular.ttf",
            "NotoSansSC-Regular.otf",
            "NotoSansTC-Regular.otf",
            "NotoSansKR-Regular.otf",
            // Source Han
            "SourceHanSans-Regular.otf",
            "SourceHanSerif-Regular.otf",
            // Others
            "WenQuanYiMicroHei.ttf",
            "DroidSansFallback.ttf",
        ];
        let emoji = vec![
            "NotoColorEmoji.ttf",
            "EmojiOneColor-SVGinOT.ttf",
            "TwemojiMozilla.ttf",
        ];
        let ui = vec!["DejaVuSans.ttf", "NotoSans-Regular.ttf", "Ubuntu-R.ttf"];
        let mono = vec![
            "DejaVuSansMono.ttf",
            "NotoSansMono-Regular.ttf",
            "UbuntuMono-R.ttf",
        ];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[allow(unreachable_code)]
    {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
    }
}

fn find_font_in_dirs(dirs: &[std::path::PathBuf], names: &[&str]) -> Option<std::path::PathBuf> {
    if dirs.is_empty() || names.is_empty() {
        return None;
    }
    let lower_names = names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut stack: Vec<std::path::PathBuf> = dirs.to_vec();
    let mut visited = 0usize;
    while let Some(p) = stack.pop() {
        if visited > 50_000 {
            break;
        } // safety cap to avoid long walks
        visited += 1;
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(file) = path.file_name().and_then(|s| s.to_str()) {
                let lf = file.to_ascii_lowercase();
                if lower_names.iter().any(|n| lf.ends_with(n)) {
                    return Some(path);
                }
            }
        }
    }
    None
}
