//! Stack spike for the Code Viewer backend: bundled SQLite, tree-sitter grammars, jj-lib 0.45.1.
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Bundled SQLite with the two extensions the store needs.
    let db = rusqlite::Connection::open_in_memory()?;
    db.execute_batch(
        "CREATE VIRTUAL TABLE r USING rtree(id, min_x, max_x, min_y, max_y);
         CREATE VIRTUAL TABLE s USING fts5(label);
         INSERT INTO r VALUES (1, 0, 10, 0, 10);
         INSERT INTO s VALUES ('EventBus.emit');",
    )?;
    let hits: i64 = db.query_row("SELECT count(*) FROM r WHERE max_x >= 5 AND min_x <= 5", [], |r| r.get(0))?;
    let fts: String = db.query_row("SELECT label FROM s WHERE s MATCH 'eventbus*'", [], |r| r.get(0))?;
    println!("sqlite {} · rtree hits {hits} · fts match {fts}", rusqlite::version());

    // 2. One grammar per target language, parsing a callsite each.
    let samples: [(&str, tree_sitter::Language, &str); 4] = [
        ("python", tree_sitter_python::LANGUAGE.into(), "def f(x):\n    return bus.emit(x)\n"),
        ("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), "export function f(id: string) { return request(`/node/${id}`); }"),
        ("rust", tree_sitter_rust::LANGUAGE.into(), "fn f(x: u32) -> u32 { bus.emit(x) }"),
        ("cpp", tree_sitter_cpp::LANGUAGE.into(), "int f(int x) { return bus.emit(x); }"),
    ];
    for (name, lang, src) in samples {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang)?;
        let tree = parser.parse(src, None).ok_or("parse failed")?;
        println!("{name}: abi {} · root {} · errors {}", lang.abi_version(), tree.root_node().kind(), tree.root_node().has_error());
    }

    // 3. jj-lib at the CLI's exact version: load a workspace, read the head operation.
    let path = std::env::args().nth(1).ok_or("usage: cv-spike <workspace>")?;
    let settings = jj_lib::settings::UserSettings::from_config(jj_lib::config::StackedConfig::with_defaults())?;
    let workspace = jj_lib::workspace::Workspace::load(
        &settings,
        Path::new(&path),
        &jj_lib::default_backend_factories::default_backend_factories(),
        &jj_lib::default_backend_factories::default_working_copy_factories(),
    )?;
    let repo = pollster::block_on(workspace.repo_loader().load_at_head())?;
    println!("jj-lib: op {:?} · {} visible heads", repo.op_id(), repo.view().heads().len());
    Ok(())
}
