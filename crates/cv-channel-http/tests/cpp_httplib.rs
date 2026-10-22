//! P2 end to end with the C++ pack: cpp-httplib routes (`svr.Get`, regex and `:id` segments) meet
//! cpp-httplib client calls at one `METHOD /path` hub each; the method must agree.

use cv_channel::{FileIn, Project, Target};

const SERVER: &str = r#"#include <httplib.h>

void handle(const httplib::Request&, httplib::Response&);

int main() {
  httplib::Server svr;
  svr.Get("/api/health", [](const httplib::Request&, httplib::Response& res) {
    res.set_content("ok", "text/plain");
  });
  svr.Post(R"(/api/items/(\d+))", handle);
  svr.listen("0.0.0.0", 8080);
}
"#;

const CLIENT: &str = r#"#include <httplib.h>
#include <string>

static const std::string BASE = "/api";

void ping(const std::string& id) {
  httplib::Client cli("localhost", 8080);
  auto a = cli.Get("/api/health");
  auto b = cli.Post(BASE + "/items/" + id, "{}", "application/json");
  auto c = cli.Delete("/api/health");
}
"#;

#[test]
fn httplib_routes_meet_httplib_clients() {
    let mut langs = cv_lang::Registry::new();
    langs.register(Box::new(cv_lang_cpp::CppPack::default()));
    let mut chan = cv_channel::Registry::new();
    chan.register(Box::new(cv_channel_http::HttpPack::new()));
    let file = |path: &'static str, text: &'static str| FileIn {
        path,
        lane: "code",
        lang: Some("cpp"),
        conv: Default::default(),
        text,
        config_keys: &[],
        imports: vec![],
        symbols: vec![],
    };
    let files = vec![
        file("src/server.cpp", SERVER),
        file("src/client.cpp", CLIENT),
    ];
    let project = Project::new(&files);
    let out = chan.run(&langs, &project, &[Some(0), Some(0)]);

    let mut hubs: Vec<&str> = out.hubs.iter().map(|h| h.display.as_str()).collect();
    hubs.sort_unstable();
    assert_eq!(
        hubs,
        vec!["GET /api/health", "POST /api/items/{}"],
        "{:?}",
        out.notes
    );

    // Client line → route line, across files.
    let mut links: Vec<(usize, u32, usize, u32)> = out
        .links
        .iter()
        .filter_map(|l| match l.to {
            Target::Site { file, line } => Some((l.from_file, l.from_line, file, line)),
            _ => None,
        })
        .collect();
    links.sort_unstable();
    assert_eq!(links, vec![(1, 8, 0, 7), (1, 9, 0, 10)]);
    assert!(out.links.iter().all(|l| l.kind == "http-request"));
    // `cli.Delete("/api/health")` has no DELETE route: it stays unlinked.
    assert!(!out.links.iter().any(|l| l.from_line == 10));
}
