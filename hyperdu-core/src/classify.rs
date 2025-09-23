use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{filters::path_excluded, Options};
use serde::Serialize;

#[derive(Clone, Copy, Debug)]
pub enum ClassifyMode {
    Basic,
    Deep,
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct CategoryStats {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Default, Debug, Clone)]
pub struct TypeStatistics {
    pub by_category: HashMap<String, CategoryStats>,
    pub by_extension: HashMap<String, CategoryStats>,
    pub top_consumers: BTreeMap<u64, Vec<PathBuf>>, // size -> paths
}

impl TypeStatistics {
    pub fn add(&mut self, path: &Path, ext: &str, cat: &str, size: u64) {
        let e = self.by_extension.entry(ext.to_string()).or_default();
        e.files += 1;
        e.bytes += size;
        let c = self.by_category.entry(cat.to_string()).or_default();
        c.files += 1;
        c.bytes += size;
        self.top_consumers.entry(size).or_default().push(path.to_path_buf());
        if self.top_consumers.len() > 2048 {
            // cap memory by trimming smallest buckets
            while self.top_consumers.len() > 1024 {
                if let Some(k) = self.top_consumers.keys().next().cloned() {
                    self.top_consumers.remove(&k);
                } else { break; }
            }
        }
    }
}

fn basic_category_from_ext(ext: &str) -> &'static str {
    let e = ext.to_ascii_lowercase();
    match e.as_str() {
        // media
        "jpg"|"jpeg"|"png"|"gif"|"webp"|"bmp"|"tiff"|"heic" => "image",
        "mp4"|"mkv"|"mov"|"avi"|"wmv"|"webm" => "video",
        "mp3"|"flac"|"aac"|"wav"|"ogg"|"m4a" => "audio",
        // documents
        "pdf"|"doc"|"docx"|"xls"|"xlsx"|"ppt"|"pptx"|"odt"|"md"|"txt" => "document",
        // archives
        "zip"|"7z"|"rar"|"tar"|"gz"|"bz2"|"xz"|"zst" => "archive",
        // code
        "rs"|"c"|"cpp"|"h"|"hpp"|"py"|"js"|"ts"|"go"|"java"|"kt"|"swift"|"rb"|"php"|"cs"|"sh" => "source",
        // packages/binaries
        "so"|"dll"|"dylib"|"exe"|"bin"|"deb"|"rpm"|"appimage" => "binary",
        _ => "other",
    }
}

fn deep_category_from_bytes(buf: &[u8]) -> &'static str {
    if let Some(t) = infer::get(buf) {
        let mime = t.mime_type();
        if mime.starts_with("image/") { return "image"; }
        if mime.starts_with("video/") { return "video"; }
        if mime.starts_with("audio/") { return "audio"; }
        if mime == "application/pdf" || mime.starts_with("text/") { return "document"; }
        if mime == "application/zip"
            || mime == "application/x-7z-compressed"
            || mime == "application/x-rar-compressed"
            || mime == "application/x-xz"
            || mime == "application/gzip"
            || mime.contains("x-tar")
        { return "archive"; }
        if mime.starts_with("application/vnd.openxmlformats-officedocument")
            || mime == "application/msword"
            || mime == "application/vnd.ms-excel"
            || mime == "application/vnd.ms-powerpoint"
        { return "document"; }
        if mime == "application/x-executable" || mime == "application/x-sharedlib" { return "binary"; }
        return "other";
    }
    "other"
}

pub fn classify_directory(root: &Path, opt: &Options, mode: ClassifyMode) -> TypeStatistics {
    let mut stats = TypeStatistics::default();
    fn walk(dir: &Path, depth: u32, opt: &Options, mode: ClassifyMode, stats: &mut TypeStatistics) {
        if opt.max_depth > 0 && depth > opt.max_depth { return; }
        let rd = match fs::read_dir(dir) { Ok(r) => r, Err(_) => return };
        for ent in rd {
            let Ok(ent) = ent else { continue };
            let path = ent.path();
            if path_excluded(&path, opt) { continue; }
            let Ok(md) = ent.metadata() else { continue };
            if md.is_dir() {
                walk(&path, depth + 1, opt, mode, stats);
            } else if md.is_file() {
                let size = md.len();
                if size < opt.min_file_size { continue; }
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                let mut cat = basic_category_from_ext(ext);
                if let ClassifyMode::Deep = mode {
                    if size > 0 {
                        let mut f = match fs::File::open(&path) { Ok(f) => f, Err(_) => continue };
                        let mut buf = [0u8; 8192];
                        let n = match f.read(&mut buf) { Ok(n) => n, Err(_) => 0 };
                        cat = deep_category_from_bytes(&buf[..n]);
                    }
                }
                stats.add(&path, ext, cat, size);
            }
        }
    }
    walk(root, 0, opt, mode, &mut stats);
    stats
}
