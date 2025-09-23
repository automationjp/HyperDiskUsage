use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Job {
    pub dir: PathBuf,
    pub depth: u32,
    pub resume: Option<u64>,
}

