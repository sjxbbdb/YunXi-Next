use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_WEB_ASSET_BYTES: u64 = 1024 * 1024;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let web_dist = manifest_dir.join("../../web/dist");
    let index = web_dist.join("index.html");
    if !index.is_file() {
        panic!("dsh Web assets are missing; run web/scripts/import-dsh-web.ps1 before building");
    }

    println!("cargo:rerun-if-changed={}", web_dist.display());
    let mut files = Vec::new();
    collect_files(&web_dist, &mut files);
    files.sort();

    let mut generated = String::from(
        "pub(crate) fn embedded_web_asset(path: &str) -> Option<EmbeddedWebAsset> {\n    match path {\n",
    );
    for file in files {
        let metadata = fs::metadata(&file)
            .unwrap_or_else(|error| panic!("cannot inspect {}: {error}", file.display()));
        if metadata.len() > MAX_WEB_ASSET_BYTES {
            panic!(
                "Web asset {} is {} bytes; the per-file limit is {} bytes",
                file.display(),
                metadata.len(),
                MAX_WEB_ASSET_BYTES
            );
        }

        let relative = file
            .strip_prefix(&web_dist)
            .expect("asset must remain below web/dist");
        let request_path = format!("/{}", slash_path(relative));
        let content_type = content_type(&file);
        let cache_control =
            if request_path.starts_with("/assets/") || request_path.starts_with("/plugins/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
        let absolute = fs::canonicalize(&file)
            .unwrap_or_else(|error| panic!("cannot resolve {}: {error}", file.display()));
        let absolute_literal = format!("{:?}", absolute.to_string_lossy());
        generated.push_str(&format!(
            "        {request_path:?} => Some(EmbeddedWebAsset {{ content_type: {content_type:?}, cache_control: {cache_control:?}, body: include_bytes!({absolute_literal}) }}),\n"
        ));
        println!("cargo:rerun-if-changed={}", file.display());
    }
    generated.push_str("        _ => None,\n    }\n}\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("web_assets.rs"), generated).expect("write generated Web asset table");
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("read Web asset directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("read Web asset file type");
        if file_type.is_dir() {
            collect_files(&path, files);
        } else if file_type.is_file() {
            content_type(&path);
            files.push(path);
        } else {
            panic!("unsupported Web asset entry: {}", path.display());
        }
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(OsStr::to_str) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("webmanifest") => "application/manifest+json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        extension => panic!(
            "unsupported Web asset extension {:?}: {}",
            extension,
            path.display()
        ),
    }
}
