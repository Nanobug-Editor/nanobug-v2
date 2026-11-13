//! Verify a real parser child reuses only the exact document revision.

use nanobug_plugin_sdk::{DocumentSnapshot, Reply, Request, receive, send};
use serde_json::Value;
use std::{
    io::BufReader,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

fn worker() -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanobug-plugin-runtime"));
    command
        .arg("syntax")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().unwrap();
    let input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    assert!(matches!(
        receive::<Reply>(&mut output).unwrap(),
        Some(Reply::Hello { .. })
    ));
    (child, input, output)
}

fn parse(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    id: u64,
    language: &str,
    document: DocumentSnapshot,
) -> Value {
    send(
        input,
        &Request::Parse {
            id,
            language: language.into(),
            document: Box::new(document),
        },
    )
    .unwrap();
    let Some(Reply::Parsed {
        id: received,
        value,
    }) = receive::<Reply>(output).unwrap()
    else {
        panic!("Expected the syntax result");
    };
    assert_eq!(received, id);
    value
}

#[test]
fn parser_reuses_matching_version_and_reparses_changed_text() {
    let (mut child, mut input, mut output) = worker();
    for (id, revision, text, cached) in [
        (1, 0, r#"{"汉":1}"#, false),
        (2, 0, r#"{"汉":1}"#, true),
        (3, 1, "[", false),
    ] {
        let value = parse(
            &mut input,
            &mut output,
            id,
            "json",
            DocumentSnapshot {
                opening: 1,
                revision,
                path: None,
                text: text.into(),
            },
        );
        assert_eq!(value["cached"], cached);
        assert_eq!(value["has_error"], revision == 1);
        assert_eq!(value["nodes"][0]["end"], text.len());
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn vue_ranges_track_unsaved_edits_and_document_openings() {
    let (mut child, mut input, mut output) = worker();
    let path = Some(PathBuf::from("Example.vue"));
    let before = r#"<template>
  <p>你好👩‍💻é</p>
</template>
<script setup lang="ts">
const count: number = 1;
</script>
<style scoped>
p { color: red; }
</style>"#;
    let after = before.replace("你好", "こんにちは").replace("= 1", "= 2");
    let inputs = [
        ("vue", 1, 0, before, false),
        ("vue", 1, 0, before, true),
        ("vue", 1, 1, after.as_str(), false),
        ("vue", 2, 0, after.as_str(), false),
        ("html", 2, 0, after.as_str(), false),
    ];
    for (index, (language, opening, revision, text, cached)) in inputs.into_iter().enumerate() {
        let value = parse(
            &mut input,
            &mut output,
            index as u64 + 1,
            language,
            DocumentSnapshot {
                opening,
                revision,
                path: path.clone(),
                text: text.into(),
            },
        );
        assert_eq!(value["opening"], opening);
        assert_eq!(value["revision"], revision);
        assert_eq!(value["cached"], cached);
        assert_eq!(value["embedded_complete"], true);
        let embedded = value["embedded"].as_array().unwrap();
        let mut expected = vec![
            (
                "typescript",
                "<script setup lang=\"ts\">",
                "</script>",
                "lexical_declaration",
            ),
            ("css", "<style scoped>", "</style>", "rule_set"),
        ];
        if language == "vue" {
            expected.insert(0, ("html", "<template>", "</template>", "element"));
        }
        assert_eq!(embedded.len(), expected.len());
        for (region, (grammar, open, close, kind)) in embedded.iter().zip(expected) {
            let start = text.find(open).unwrap() + open.len();
            let end = text.find(close).unwrap();
            assert_eq!(region["language"], grammar);
            assert_eq!(region["start"], start);
            assert_eq!(region["end"], end);
            assert_eq!(region["has_error"], false);
            assert!(region["nodes"].as_array().unwrap().iter().any(|node| {
                node["kind"] == kind
                    && node["start"].as_u64().unwrap() >= start as u64
                    && node["end"].as_u64().unwrap() <= end as u64
            }));
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
