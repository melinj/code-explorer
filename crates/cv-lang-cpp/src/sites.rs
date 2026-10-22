//! Channel endpoint sites for C and C++ (DESIGN §4.6): runs the channel packs' queries
//! (`lang = "cpp"` rules such as `cpp-httplib.toml` and `cpp-getenv.toml`) and folds the captured
//! keys with this pack's folder (literals, concatenation, raw strings, file-local constants).

use cv_lang::keys::{FoldTransform, KeyExpr, KeyPart, SiteExtractor, SiteFacts, SiteQuery};
use cv_lang_sitter::Text;
use cv_lang_sitter::sites::CompiledSites;

use crate::{Folder, finish, parse};

pub struct CppSites {
    compiled: CompiledSites,
}

impl CppSites {
    pub fn new(queries: &[SiteQuery], _transforms: &[FoldTransform]) -> Self {
        CppSites {
            compiled: CompiledSites::new(&tree_sitter_cpp::LANGUAGE.into(), queries),
        }
    }
}

impl SiteExtractor for CppSites {
    fn extract(&self, _path: &str, source: &str) -> SiteFacts {
        if self.compiled.is_empty() || !self.compiled.wants(source) {
            return SiteFacts::default();
        }
        let Some(tree) = parse(source) else {
            return SiteFacts::default();
        };
        let root = tree.root_node();
        let text = Text::new(source);
        let folder = Folder { root, src: source };
        let sites = self.compiled.run(root, &text, &mut |n| {
            let mut parts = Vec::new();
            folder.fold(n, &mut parts, 0);
            let parts = finish(parts, folder.text(n));
            let mut k = KeyExpr::default();
            for p in parts {
                k.push(match p {
                    crate::KeyPart::Lit(s) => KeyPart::Lit(s),
                    crate::KeyPart::Segment => KeyPart::Segment,
                    crate::KeyPart::Unknown(h) => KeyPart::Unknown {
                        hint: cv_lang::keys::shorten(&h),
                    },
                });
            }
            k
        });
        // File-local constants are folded in place; nothing is exported to other files.
        SiteFacts {
            sites,
            consts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_route_and_client_paths() {
        let src = r#"#include <httplib.h>
#define API "/api"
int main() {
  httplib::Server svr;
  svr.Get(API "/items", [](const httplib::Request&, httplib::Response& res) {});
  httplib::Client cli("localhost", 8080);
  auto r = cli.Post(std::string(API) + "/items/" + id, body, "application/json");
}
"#;
        let queries = [SiteQuery {
            id: "q".into(),
            query: r#"(call_expression
  function: (field_expression field: (field_identifier) @method)
  arguments: (argument_list . (_) @key)) @line"#
                .into(),
            hints: vec![],
        }];
        let s = CppSites::new(&queries, &[]).extract("main.cpp", src);
        let got: Vec<(String, u32, Option<&str>)> = s
            .sites
            .iter()
            .map(|x| (x.key.render(), x.line, x.method.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("/api/items".to_string(), 5, Some("Get")),
                ("/api/items/{}".to_string(), 7, Some("Post")),
            ]
        );
    }
}
