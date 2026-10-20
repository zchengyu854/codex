use std::path::{Path, PathBuf};

pub const CHECK: [&str; 8] = [
    "/usr/bin/python3",
    "-B",
    "-m",
    "unittest",
    "discover",
    "-s",
    "tests",
    "-v",
];

pub fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/stage8-project")
}

pub fn overlay(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            overlay(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

pub fn patch(index: usize) -> serde_json::Value {
    let (change, module) = [("slug", "slug"), ("total", "total"), ("report", "report")][index];
    let root = fixture().join("changes").join(change);
    let source = format!("{module}.py");
    let test = format!("tests/test_{module}.py");
    serde_json::json!({
        source.as_str(): std::fs::read_to_string(root.join(&source)).unwrap(),
        test.as_str(): std::fs::read_to_string(root.join(&test)).unwrap(),
    })
}
