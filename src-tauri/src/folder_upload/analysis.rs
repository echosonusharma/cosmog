//! Analysis aggregates: built during the walk, stored as `analysis_json`, served to the UI.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

use serde::{Deserialize, Serialize};

use crate::db::folder_upload::ExcludeReason;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtStat {
    pub ext: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirStat {
    pub rel_path: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStat {
    pub rel_path: String,
    pub size: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HiddenStat {
    pub files: i64,
    pub bytes: i64,
    pub top_dirs: Vec<DirStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deepest {
    pub rel_path: String,
    pub depth: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderAnalysis {
    pub job_id: String,
    pub source_root: String,
    pub dest_prefix: String,
    pub encrypted: bool,
    pub files: i64,
    pub dirs: i64,
    pub empty_dirs: i64,
    pub total_bytes: i64,
    pub zero_byte_files: i64,
    pub by_ext: Vec<ExtStat>,
    pub hidden: HiddenStat,
    pub top_dirs: Vec<DirStat>,
    pub largest_files: Vec<FileStat>,
    pub excluded: BTreeMap<String, i64>,
    pub windows_unsafe_names: i64,
    pub deepest: Option<Deepest>,
    pub longest_rel_bytes: i64,
    pub largest_file_bytes: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FolderAnalyzeEvent {
    Started { job_id: String },
    Progress { job_id: String, files: i64, dirs: i64, bytes: i64, current_dir: String },
}

const TOP_EXT: usize = 200;
const TOP_N: usize = 10;

/// Running totals of one walk.
#[derive(Default)]
pub struct Stats {
    pub files: i64,
    pub dirs: i64,
    pub empty_dirs: i64,
    pub total_bytes: i64,
    pub zero_byte_files: i64,
    by_ext: HashMap<String, (i64, i64)>,
    hidden_files: i64,
    hidden_bytes: i64,
    hidden_dirs: HashMap<String, (i64, i64)>,
    top_dirs: HashMap<String, (i64, i64)>,
    largest: BinaryHeap<Reverse<(i64, String)>>,
    excluded: HashMap<ExcludeReason, i64>,
    pub windows_unsafe_names: i64,
    deepest: Option<(i64, String)>,
    longest_rel_bytes: i64,
    largest_file_bytes: i64,
}

impl Stats {
    /// An uploadable file; `hidden_dir` is its topmost hidden ancestor (or itself when hidden).
    pub fn add_file(&mut self, rel: &str, size: i64, ext: Option<&str>, hidden_dir: Option<&str>, hidden: bool) {
        self.files += 1;
        self.total_bytes += size;
        if size == 0 {
            self.zero_byte_files += 1;
        }
        let e = self.by_ext.entry(ext.unwrap_or("").to_string()).or_default();
        e.0 += 1;
        e.1 += size;
        if hidden {
            self.hidden_files += 1;
            self.hidden_bytes += size;
            if let Some(d) = hidden_dir {
                let h = self.hidden_dirs.entry(d.to_string()).or_default();
                h.0 += 1;
                h.1 += size;
            }
        }
        if let Some((top, _)) = rel.split_once('/') {
            let t = self.top_dirs.entry(top.to_string()).or_default();
            t.0 += 1;
            t.1 += size;
        }
        self.largest.push(Reverse((size, rel.to_string())));
        if self.largest.len() > TOP_N {
            self.largest.pop();
        }
        self.largest_file_bytes = self.largest_file_bytes.max(size);
        self.see_path(rel);
    }

    pub fn add_dir(&mut self, rel: &str) {
        self.dirs += 1;
        self.see_path(rel);
    }

    pub fn add_excluded(&mut self, reason: ExcludeReason) {
        *self.excluded.entry(reason).or_default() += 1;
    }

    fn see_path(&mut self, rel: &str) {
        let depth = rel.split('/').count() as i64;
        if self.deepest.as_ref().map_or(true, |(d, _)| depth > *d) {
            self.deepest = Some((depth, rel.to_string()));
        }
        self.longest_rel_bytes = self.longest_rel_bytes.max(rel.len() as i64);
    }

    pub fn finish(self, job_id: &str, root: &str, dest: &str, encrypted: bool) -> FolderAnalysis {
        let mut by_ext: Vec<ExtStat> =
            self.by_ext.into_iter().map(|(ext, (files, bytes))| ExtStat { ext, files, bytes }).collect();
        by_ext.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.files.cmp(&a.files)).then(a.ext.cmp(&b.ext)));
        if by_ext.len() > TOP_EXT {
            let rest = by_ext.split_off(TOP_EXT);
            let (files, bytes) = rest.iter().fold((0, 0), |(f, b), e| (f + e.files, b + e.bytes));
            by_ext.push(ExtStat { ext: "*".into(), files, bytes });
        }
        let mut largest: Vec<FileStat> =
            self.largest.into_iter().map(|Reverse((size, rel_path))| FileStat { rel_path, size }).collect();
        largest.sort_by(|a, b| b.size.cmp(&a.size).then(a.rel_path.cmp(&b.rel_path)));
        let excluded: BTreeMap<String, i64> = ExcludeReason::ALL
            .iter()
            .map(|r| (r.as_str().to_string(), self.excluded.get(r).copied().unwrap_or(0)))
            .collect();
        let mut a = FolderAnalysis {
            job_id: job_id.into(),
            source_root: root.into(),
            dest_prefix: dest.into(),
            encrypted,
            files: self.files,
            dirs: self.dirs,
            empty_dirs: self.empty_dirs,
            total_bytes: self.total_bytes,
            zero_byte_files: self.zero_byte_files,
            by_ext,
            hidden: HiddenStat { files: self.hidden_files, bytes: self.hidden_bytes, top_dirs: top(self.hidden_dirs) },
            top_dirs: top(self.top_dirs),
            largest_files: largest,
            excluded,
            windows_unsafe_names: self.windows_unsafe_names,
            deepest: self.deepest.map(|(depth, rel_path)| Deepest { rel_path, depth }),
            longest_rel_bytes: self.longest_rel_bytes,
            largest_file_bytes: self.largest_file_bytes,
            warnings: Vec::new(),
        };
        a.warnings = warnings(&a);
        a
    }
}

fn top(map: HashMap<String, (i64, i64)>) -> Vec<DirStat> {
    let mut v: Vec<DirStat> = map.into_iter().map(|(rel_path, (files, bytes))| DirStat { rel_path, files, bytes }).collect();
    v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.files.cmp(&a.files)).then(a.rel_path.cmp(&b.rel_path)));
    v.truncate(TOP_N);
    v
}

pub fn warnings(a: &FolderAnalysis) -> Vec<String> {
    let mut w = Vec::new();
    if a.files > super::WARN_FILES {
        w.push("many_files");
    }
    if a.total_bytes > super::WARN_BYTES {
        w.push("large_total");
    }
    if a.hidden.files > 0 {
        w.push("hidden_present");
    }
    if a.excluded.values().any(|n| *n > 0) {
        w.push("excluded_present");
    }
    if a.encrypted && a.files > 0 {
        w.push("encrypted_temp_space");
    }
    w.into_iter().map(String::from).collect()
}
