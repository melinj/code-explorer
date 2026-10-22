//! The web UI, served by `cv serve` itself from a built `web/dist`, so one command runs everything.
//! The UI talks to `/api` on its own origin; static files need no token (the page reads it from
//! `?token=` and sends it on every API call).

use std::path::{Path, PathBuf};

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

/// Where the built UI is: `--ui DIR`, else `CV_UI_DIR`, else `web/dist` of the checkout this binary
/// was built in (`target/{release,debug}/cv` → `../../web/dist`), else `./web/dist`. `None` when none
/// of those holds an `index.html`.
pub fn find_dir(flag: Option<String>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(f) = flag {
        candidates.push(f.into());
    }
    if let Some(e) = std::env::var_os("CV_UI_DIR").filter(|e| !e.is_empty()) {
        candidates.push(e.into());
    }
    if let Ok(exe) = std::env::current_exe().and_then(|p| p.canonicalize()) {
        for up in exe.ancestors().skip(1).take(4) {
            candidates.push(up.join("web/dist"));
        }
    }
    candidates.push("web/dist".into());
    candidates
        .into_iter()
        .find(|d| d.join("index.html").is_file())
}

/// Static files from `dir`; unknown paths get `index.html` (the app has one page).
pub fn router(dir: &Path) -> Router {
    Router::new()
        .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))))
}
