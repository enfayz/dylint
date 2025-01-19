#![feature(rustc_private)]
#![feature(let_chains)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use clippy_utils::{attrs::is_doc_hidden, diagnostics::span_lint_and_then, source::snippet_opt};
use rustc_ast::AttrKind;
use rustc_hir::{FnSig, Item, ItemKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{BytePos, SourceFileAndLine, Span};
use serde::Deserialize;
use std::{
    fmt::Write,
    io::{Error as IoError, Read},
};

// smoelius: We should consider switching to `async_openai` here:
// https://docs.rs/async-openai/latest/async_openai/
mod openai;

const OPENAI_API_KEY: &str = "OPENAI_API_KEY";

const URL: &str = "https://api.openai.com/v1/completions";

const DEFAULT_PROMPT: &str = "An elaborate, high quality rustdoc comment for the above function:";
const DEFAULT_MODEL: &str = "code-davinci-002";
const DEFAULT_MAX_TOKENS: u32 = 1000;
const DEFAULT_TEMPERATURE: f32 = 0.2;

const MOCK_COMPLETION: &str = "/// A doc comment generated by OpenAI.\n";

const STOP: &str = "\n```";

dylint_linting::impl_late_lint! {
    /// ⚠️ DO NOT RUN THIS LINT ON PRIVATE SOURCE CODE ⚠️
    ///
    /// ### What it does
    /// Checks for functions missing [doc comments].
    ///
    /// ### Why is this bad?
    /// Understanding what a function does is easier given a description of the function rather than
    /// just its code.
    ///
    /// ### Known problems
    /// The lint is currently enabled only for functions.
    ///
    /// ### Example
    /// ```rust
    /// pub fn foo() {}
    /// ```
    /// Use instead:
    /// ```rust
    /// /// A doc comment generated by OpenAI.
    /// pub fn foo() {}
    /// ```
    ///
    /// ### OpenAI
    /// If the environment variable `OPENAI_API_KEY` is set to an [OpenAI API key], the lint will
    /// suggest a doc comment generated by OpenAI. The prompt sent to OpenAI has the following form:
    /// ````ignore
    /// ```rust
    /// <function declaration>
    /// ```
    /// An elaborate, high quality rustdoc comment for the above function:
    /// ```rust
    /// ````
    /// The prompt's [`stop` parameter] is set to `["\n```"]`. Thus, OpenAI should stop generating tokens once the second code block is complete. The suggested doc comment is the one that appears in that code block, if any.
    ///
    /// The phrase "An elaborate..." is configurable (see below).
    ///
    /// ### Configuration
    /// Certain [OpenAI parameters] can be configured by setting them in the
    /// `missing_doc_comment_openai` table of the linted workspace's [`dylint.toml` file]. Example:
    /// ```toml
    /// [missing_doc_comment_openai]
    /// prompt = "A rustdoc comment for the above function with a \"Motivation\" section:"
    /// temperature = 1.0
    /// ```
    /// The following parameters are supported:
    /// - `prompt` (default "An elaborate, high quality rustdoc comment for the above function:").
    ///   This default is based on OpenAI's [Write a Python docstring] example.
    /// - `model` (default "[code-davinci-002]")
    /// - `temperature` (default 0.2). Note that this default is less than OpenAI's default (1.0).
    ///   Per the [`temperature` documentation], "Higher values like 0.8 will make the output more
    ///   random, while lower values like 0.2 will make it more focused and deterministic."
    /// - `top_p` (default none, i.e., use OpenAI's default)
    /// - `presence_penalty` (default none, i.e., use OpenAI's default)
    /// - `frequency_penalty` (default none, i.e., use OpenAI's default)
    ///
    /// [`dylint.toml` file]: https://github.com/trailofbits/dylint#configurable-libraries
    /// [`stop` parameter]: https://platform.openai.com/docs/api-reference/completions/create#completions/create-stop
    /// [`temperature` documentation]: https://platform.openai.com/docs/api-reference/completions/create#completions/create-temperature
    /// [code-davinci-002]: https://platform.openai.com/docs/models/codex
    /// [doc comments]: https://doc.rust-lang.org/rust-by-example/meta/doc.html#doc-comments
    /// [openai api key]: https://help.openai.com/en/articles/4936850-where-do-i-find-my-secret-api-key
    /// [openai parameters]: https://platform.openai.com/docs/api-reference/completions/create
    /// [write a python docstring]: https://platform.openai.com/examples/default-python-docstring
    pub MISSING_DOC_COMMENT_OPENAI,
    Warn,
    "description goes here",
    MissingDocCommentOpenai::new()
}

#[derive(Default, Deserialize)]
struct Config {
    prompt: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    frequency_penalty: Option<f32>,
    presence_penalty: Option<f32>,
}

struct MissingDocCommentOpenai {
    config: Config,
}

impl MissingDocCommentOpenai {
    pub fn new() -> Self {
        Self {
            config: dylint_linting::config_or_default(env!("CARGO_PKG_NAME")),
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for MissingDocCommentOpenai {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        if std::env::var(OPENAI_API_KEY).is_err() {
            cx.sess().dcx().warn(format!(
                "`missing_doc_comment_openai` suggestions are disabled because environment \
                 variable `{OPENAI_API_KEY}` is not set"
            ));
        }
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let owner_id = item.owner_id;

        // smoelius: The next two checks were copied from:
        // https://github.com/rust-lang/rust-clippy/blob/92c4f1e2d9db43ebc0449fbbc2150eeb9429e65b/clippy_lints/src/doc.rs#L372-L384

        if !cx.effective_visibilities.is_exported(owner_id.def_id) {
            return; // Private functions do not require doc comments
        }

        // do not lint if any parent has `#[doc(hidden)]` attribute (#7347)
        if cx
            .tcx
            .hir()
            .parent_iter(owner_id.into())
            .any(|(id, _node)| is_doc_hidden(cx.tcx.hir().attrs(id)))
        {
            return;
        }

        // smoelius: Only enable for functions for now.
        let ItemKind::Fn(
            FnSig {
                span: fn_sig_span, ..
            },
            _,
            _,
        ) = item.kind
        else {
            return;
        };

        if cx
            .tcx
            .hir()
            .attrs(item.hir_id())
            .iter()
            .any(|attr| matches!(attr.kind, AttrKind::DocComment{ .. }))
        {
            return;
        }

        let doc_comment = std::env::var(OPENAI_API_KEY).ok().and_then(|api_key| {
            let snippet = snippet_opt(cx, item.span)?;

            let request = self.request_from_snippet(&snippet);

            let response = match send_request(&api_key, &request) {
                Ok(response) => response,
                Err(error) => {
                    cx.sess().dcx().span_warn(fn_sig_span, error.to_string());
                    return None;
                }
            };

            response
                .choices
                .first()
                .and_then(|choice| extract_doc_comment(&choice.text))
                .or_else(|| {
                    cx.sess().dcx().span_warn(
                        fn_sig_span,
                        format!("Could not extract doc comment from response: {response:#?}",),
                    );
                    None
                })
        });

        let insertion_point = skip_preceding_line_comments(cx, earliest_attr_span(cx, item));

        span_lint_and_then(
            cx,
            MISSING_DOC_COMMENT_OPENAI,
            fn_sig_span,
            "exported function lacks a doc comment",
            |diag| {
                if let Some(doc_comment) = doc_comment {
                    diag.span_suggestion(
                        insertion_point.with_hi(insertion_point.lo()),
                        "use the following suggestion from OpenAI",
                        doc_comment,
                        rustc_errors::Applicability::MachineApplicable,
                    );
                }
            },
        );
    }
}

impl MissingDocCommentOpenai {
    fn request_from_snippet(&self, snippet: &str) -> openai::Request {
        openai::Request {
            prompt: self.prompt_from_snippet(snippet),
            model: self
                .config
                .model
                .as_deref()
                .unwrap_or(DEFAULT_MODEL)
                .to_owned(),
            max_tokens: self.config.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            temperature: self.config.temperature.unwrap_or(DEFAULT_TEMPERATURE),
            top_p: self.config.top_p,
            frequency_penalty: self.config.frequency_penalty,
            presence_penalty: self.config.presence_penalty,
            stop: &[STOP],
        }
    }

    fn prompt_from_snippet(&self, snippet: &str) -> String {
        format!(
            "```rust\n{snippet}\n```\n{}\n```rust\n",
            self.config.prompt.as_deref().unwrap_or(DEFAULT_PROMPT)
        )
    }
}

fn send_request(api_key: &str, request: &openai::Request) -> Result<openai::Response, IoError> {
    if testing() {
        return Ok(openai::Response {
            choices: vec![openai::Choice {
                text: MOCK_COMPLETION.to_owned(),
                index: 0,
                finish_reason: "stop".to_owned(),
            }],
            ..Default::default()
        });
    }

    serde_json::to_vec(&request)
        .map_err(IoError::from)
        .and_then(|data| {
            debug("request", &data);
            send(api_key, &data).map_err(IoError::from)
        })
        .and_then(|(code, data)| {
            debug("response", &data);
            if code == 200 {
                serde_json::from_slice(&data).map_err(IoError::from)
            } else {
                match std::str::from_utf8(&data) {
                    Ok(error) => Err(IoError::other(format!("{code}: {error}"))),
                    Err(error) => Err(IoError::other(format!("{code}: {error}"))),
                }
            }
        })
}

fn debug(label: &str, data: &[u8]) {
    if enabled("DEBUG") {
        let s = match std::str::from_utf8(data) {
            Ok(s) => s.to_owned(),
            Err(error) => error.to_string(),
        };
        println!("{label}: {s}");
    }
}

fn send(api_key: &str, mut data: &[u8]) -> Result<(u32, Vec<u8>), IoError> {
    let mut list = curl::easy::List::new();
    list.append("Content-Type: application/json")?;
    list.append(&format!("Authorization: Bearer {api_key}"))?;

    let mut handle = curl::easy::Easy::new();
    handle.post(true)?;
    handle.url(URL)?;
    handle.http_headers(list)?;

    let mut response = Vec::new();
    {
        let mut transfer = handle.transfer();
        transfer.read_function(|dst| {
            let len = data.read(dst).unwrap();
            Ok(len)
        })?;
        transfer.write_function(|src| {
            response.extend_from_slice(src);
            Ok(src.len())
        })?;
        transfer.perform()?;
    }

    let code = handle.response_code()?;

    Ok((code, response))
}

fn extract_doc_comment(response: &str) -> Option<String> {
    // smoelius: Sanity. According to:
    // https://platform.openai.com/docs/api-reference/completions/create#completions/create-stop
    //
    //   The returned text will not contain the stop sequence.
    assert_ne!(response.lines().last(), Some(STOP));

    // smoelius: In several of my experiments, the last several lines of the response did not start
    // with `///`. Ignore those lines. Also, in some of my experiments, the the generated comments
    // were internal attributes, i.e., started with `//!`. Convert those to external attributes.
    let mut comment = String::new();
    for line in response
        .lines()
        .take_while(|line| line.starts_with("//!") || line.starts_with("///"))
    {
        if let Some(s) = line.strip_prefix("//!") {
            writeln!(&mut comment, "///{s}").unwrap();
        } else {
            writeln!(&mut comment, "{line}").unwrap();
        }
    }

    if comment.is_empty() {
        None
    } else {
        Some(comment)
    }
}

fn earliest_attr_span(cx: &LateContext<'_>, item: &Item<'_>) -> Span {
    cx.tcx
        .hir()
        .attrs(item.hir_id())
        .iter()
        .map(|attr| attr.span)
        .fold(
            item.span,
            |lhs, rhs| if lhs.lo() <= rhs.lo() { lhs } else { rhs },
        )
}

fn skip_preceding_line_comments(cx: &LateContext<'_>, mut span: Span) -> Span {
    while span.lo() >= BytePos(1) {
        let SourceFileAndLine { sf, line } = cx
            .sess()
            .source_map()
            .lookup_line(span.lo() - BytePos(1))
            .unwrap();
        let lo_prev_relative = sf.lines()[line];
        let lo_prev = sf.absolute_position(lo_prev_relative);
        let span_prev = span.with_lo(lo_prev);
        if snippet_opt(cx, span_prev).is_some_and(|snippet| snippet.starts_with("//")) {
            span = span_prev;
        } else {
            break;
        }
    }
    span
}

#[must_use]
fn enabled(name: &str) -> bool {
    let key = env!("CARGO_PKG_NAME").to_uppercase() + "_" + name;
    std::env::var(key).is_ok_and(|value| value != "0")
}

#[must_use]
fn testing() -> bool {
    std::env::var(OPENAI_API_KEY).is_ok_and(|value| value.to_lowercase().contains("test"))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::env::set_var;

    #[test]
    fn ui() {
        set_var(OPENAI_API_KEY, "test");

        let toml = format!(
            r#"
[missing_doc_comment_openai]
prompt = "{DEFAULT_PROMPT}"
meld = "{DEFAULT_MODEL}"
max_tokens = {DEFAULT_MAX_TOKENS}
temperature = {DEFAULT_TEMPERATURE}
top_p = 1.0
frequency_penalty = 0.0
presence_penalty = 0.0
"#
        );

        dylint_testing::ui::Test::src_base(env!("CARGO_PKG_NAME"), "ui")
            .dylint_toml(toml)
            .run();
    }
}
