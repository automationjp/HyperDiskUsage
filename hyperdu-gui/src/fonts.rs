//! Platform font fallback.
//!
//! egui ships a Latin-only default face, so a Japanese path renders as boxes
//! until a CJK face is registered. This walks the platform's font directories
//! once at startup and installs whatever it finds as a fallback chain.
//! Moved out of the UI module unchanged: it is startup configuration, not
//! rendering, and it was half the file.

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};

use crate::i18n::Lang;

/// `lang` decides which CJK face comes first. Japanese and Simplified Chinese
/// share most code points but not glyph shapes, and a Japanese face lacks many
/// simplified characters, so both are installed and the UI language leads.
pub fn configure_fonts(ctx: &egui::Context, lang: Lang) {
    // Start from egui defaults and add UTF-8 capable system fallbacks (CJK, Emoji).
    let mut fonts = FontDefinitions::default();

    // Helper to add a font file if present
    let mut add_font_file = |key: &str, path: &std::path::Path| -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                let data = FontData::from_owned(bytes).tweak(tweak_for(path));
                fonts.font_data.insert(key.to_string(), data.into());
                true
            }
            Err(_) => false,
        }
    };

    // Collect candidate font files per platform
    let (dirs, cjk_candidates, sc_candidates, emoji_candidates, ui_candidates, mono_candidates) =
        platform_font_candidates();

    // Find first matches
    let find_first = |names: &[&str]| find_font_in_dirs(&dirs, names);

    let ja = fontconfig_cjk("ja").or_else(|| find_first(&cjk_candidates));
    let sc = fontconfig_cjk("zh-cn").or_else(|| find_first(&sc_candidates));
    let order = if lang == Lang::Zh { [sc, ja] } else { [ja, sc] };
    let mut added: Vec<std::path::PathBuf> = Vec::new();
    for p in order.into_iter().flatten() {
        if added.contains(&p) {
            continue;
        }
        let key = format!("cjk{}", added.len());
        if add_font_file(&key, &p) {
            added.push(p);
            // Append CJK fallback
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push(key.clone());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push(key);
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

/// Meiryo and Yu Gothic draw their glyphs higher than Segoe UI, which renders the
/// Latin text, so a mixed label such as `JSONへ保存` sat on two baselines.
/// Measured on Windows 11; Microsoft YaHei and Noto CJK line up as they are.
fn tweak_for(path: &std::path::Path) -> FontTweak {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let raised = name.starts_with("meiryo") || name.starts_with("yugoth");
    FontTweak {
        y_offset_factor: if raised { JA_WINDOWS_Y_OFFSET } else { 0.0 },
        ..FontTweak::default()
    }
}

const JA_WINDOWS_Y_OFFSET: f32 = 0.08;

/// Directories to search, then file-name candidates for each script group:
/// CJK (Japanese first), Simplified Chinese, emoji, symbols, and a Latin fallback.
type FontCandidates = (
    Vec<std::path::PathBuf>,
    Vec<&'static str>,
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
            "meiryo.ttc",
            "YuGothR.ttc",
            "YuGothM.ttc",
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
        let sc = vec!["msyh.ttc", "msyh.ttf", "Microsoft YaHei.ttf", "SimSun.ttc"];
        let emoji = vec!["seguiemj.ttf", "SegoeUIEmoji.ttf"]; // Windows emoji
        let ui = vec!["segoeui.ttf", "YuGothUI.ttc", "meiryo.ttc"];
        let mono = vec![
            "consola.ttf",
            "CascadiaMono.ttf",
            "CascadiaCode.ttf",
            "msmincho.ttc",
        ];
        return (dirs, cjk, sc, emoji, ui, mono);
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
        let sc = vec!["PingFang.ttc", "PingFangSC.ttc", "STHeiti Light.ttc"];
        let emoji = vec!["Apple Color Emoji.ttc", "AppleColorEmoji.ttf"];
        let ui = vec![
            "SFNS.ttf",
            "HelveticaNeueDeskInterface.ttc",
            "HiraginoSans-W3.ttc",
        ];
        let mono = vec!["Menlo.ttc", "SFMono.ttf", "OsakaMono.ttf"];
        return (dirs, cjk, sc, emoji, ui, mono);
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
        let sc = vec![
            "NotoSansCJKsc-Regular.otf",
            "NotoSansSC-Regular.otf",
            "NotoSansSC-Regular.ttf",
            "NotoSansCJK-Regular.ttc",
            "wqy-microhei.ttc",
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
        return (dirs, cjk, sc, emoji, ui, mono);
    }
    #[allow(unreachable_code)]
    {
        (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }
}

/// CJK file names differ per distribution (NotoSansCJK-VF.ttc, ipag.ttf,
/// VL-Gothic-Regular.ttf, ...), so ask fontconfig for the face it would use for
/// `lang` (`ja`, `zh-cn`). `fc-match` always answers, so the answer counts only
/// if it covers that language.
#[cfg(target_os = "linux")]
fn fontconfig_cjk(lang: &str) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{lang}", &format!("sans-serif:lang={lang}")])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let (file, langs) = text.split_once('\n')?;
    langs
        .split('|')
        .any(|l| l == lang)
        .then(|| std::path::PathBuf::from(file))
}

#[cfg(not(target_os = "linux"))]
fn fontconfig_cjk(_lang: &str) -> Option<std::path::PathBuf> {
    None
}

fn find_font_in_dirs(dirs: &[std::path::PathBuf], names: &[&str]) -> Option<std::path::PathBuf> {
    if dirs.is_empty() || names.is_empty() {
        return None;
    }
    // Candidates are in priority order, so collect every file first and pick by
    // name order; returning the first directory hit let meiryo/Yu Gothic UI win
    // over Segoe UI and render `\` as `¥`.
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
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
                files.push((file.to_ascii_lowercase(), path));
            }
        }
    }
    names.iter().find_map(|n| {
        let n = n.to_ascii_lowercase();
        files
            .iter()
            .find(|(f, _)| f.ends_with(&n))
            .map(|(_, p)| p.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::{configure_fonts, find_font_in_dirs};
    use crate::i18n::Lang;

    /// Needs the machine's own CJK fonts, which CI runners lack, so it is run
    /// by hand: `cargo test -p hyperdu-gui -- --ignored every_label_glyph`.
    #[test]
    #[ignore]
    fn every_label_glyph_is_in_the_installed_fonts() {
        let source = include_str!("app.rs");
        let labels: Vec<&str> = source
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| s.chars().any(|c| c as u32 >= 0x3000))
            .collect();
        assert!(labels.len() > 100, "found only {} labels", labels.len());
        for lang in Lang::ALL {
            let ctx = egui::Context::default();
            configure_fonts(&ctx, lang);
            let _ = ctx.run(Default::default(), |_| {});
            let missing: String = ctx.fonts(|fonts| {
                let id = egui::FontId::proportional(14.0);
                labels
                    .iter()
                    .flat_map(|l| l.chars())
                    .filter(|&c| !fonts.has_glyph(&id, c))
                    .collect()
            });
            assert!(missing.is_empty(), "{lang:?} fonts lack {missing:?}");
        }
    }

    #[test]
    fn candidate_order_wins_over_directory_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a_meiryo.ttc", "z_segoeui.ttf"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let found = find_font_in_dirs(&[dir.path().to_path_buf()], &["segoeui.ttf", "meiryo.ttc"]);
        assert_eq!(found.unwrap().file_name().unwrap(), "z_segoeui.ttf");
    }
}
