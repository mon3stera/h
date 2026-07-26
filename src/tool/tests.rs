use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use super::*;

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: ToolCallId("call-1".to_owned()),
        name: name.to_owned(),
        arguments,
    }
}

fn temporary_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("h-{name}-{}", uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn read_file_defaults_to_a_bounded_first_page() {
    let path = temporary_file("bounded-read");
    let content = (1..=250)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, content).await.unwrap();

    let tool = ReadFileTool::new(FileBufferStore::default());
    let output = TypedTool::call(
        &tool,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(output.start_line, 1);
    assert_eq!(output.end_line, Some(200));
    assert_eq!(output.total_lines, None);
    assert!(output.has_more);
    assert_eq!(output.content.lines().count(), 200);
    assert!(output.content.starts_with("line 1\n"));
    assert!(output.content.ends_with("line 200"));

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn read_file_uses_one_based_inclusive_ranges() {
    let path = temporary_file("inclusive-read");
    fs::write(&path, "one\ntwo\n\nfour\nfive\n").await.unwrap();

    let tool = ReadFileTool::new(FileBufferStore::default());
    let output = TypedTool::call(
        &tool,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: Some(2),
            end_line: Some(4),
        },
    )
    .await
    .unwrap();

    assert_eq!(output.content, "two\n\nfour");
    assert_eq!(output.start_line, 2);
    assert_eq!(output.end_line, Some(4));
    assert_eq!(output.total_lines, None);
    assert!(output.has_more);

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn read_file_validates_ranges_before_reading() {
    let tool = ReadFileTool::new(FileBufferStore::default());
    let missing = temporary_file("missing").to_string_lossy().into_owned();

    for (start_line, end_line, expected) in [
        (Some(0), None, "start_line must be at least 1"),
        (
            Some(5),
            Some(4),
            "end_line must be greater than or equal to start_line",
        ),
        (
            Some(1),
            Some(MAX_READ_LINES + 1),
            "cannot read more than 200 lines at once",
        ),
    ] {
        let error = TypedTool::call(
            &tool,
            ReadFileToolArgs {
                path: missing.clone(),
                start_line,
                end_line,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), expected);
    }
}

#[tokio::test]
async fn read_file_handles_empty_and_past_eof_ranges() {
    let path = temporary_file("empty-read");
    fs::write(&path, "").await.unwrap();

    let tool = ReadFileTool::new(FileBufferStore::default());
    let output = TypedTool::call(
        &tool,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: Some(10),
            end_line: None,
        },
    )
    .await
    .unwrap();

    assert!(output.content.is_empty());
    assert_eq!(output.start_line, 10);
    assert_eq!(output.end_line, None);
    assert_eq!(output.total_lines, Some(0));
    assert!(!output.has_more);

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn file_indexes_extend_lazily_and_refresh_external_changes() {
    let path = temporary_file("index-refresh");
    let content = (1..=250)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, content).await.unwrap();
    let buffers = FileBufferStore::default();
    let reader = ReadFileTool::new(buffers.clone());

    let first = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.total_lines, None);

    let index = buffers.files.read().await.values().next().cloned().unwrap();
    let indexed = index.lock().await;
    assert_eq!(indexed.line_starts.len(), 201);
    assert!(indexed.scanned_to < fs::metadata(&path).await.unwrap().len());
    drop(indexed);

    fs::write(&path, "new content").await.unwrap();
    let refreshed = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(refreshed.content, "new content");
    assert_eq!(refreshed.total_lines, Some(1));

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn read_file_reaches_eof_and_remembers_exact_total() {
    let path = temporary_file("known-total");
    fs::write(&path, "one\ntwo\nthree").await.unwrap();
    let buffers = FileBufferStore::default();
    let reader = ReadFileTool::new(buffers.clone());

    let output = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: Some(2),
            end_line: Some(3),
        },
    )
    .await
    .unwrap();
    assert_eq!(output.content, "two\nthree");
    assert_eq!(output.total_lines, Some(3));
    assert!(!output.has_more);

    let index = buffers.files.read().await.values().next().cloned().unwrap();
    assert_eq!(index.lock().await.total_lines, Some(3));

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn read_file_normalizes_crlf_and_preserves_blank_lines() {
    let path = temporary_file("line-semantics");
    fs::write(&path, b"one\r\n\r\nthree\r\n").await.unwrap();
    let reader = ReadFileTool::new(FileBufferStore::default());

    let output = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(output.content, "one\n\nthree");
    assert_eq!(output.total_lines, Some(3));

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn read_file_rejects_invalid_utf8_in_scanned_lines() {
    let path = temporary_file("invalid-utf8");
    fs::write(&path, [0xff, b'\n']).await.unwrap();
    let reader = ReadFileTool::new(FileBufferStore::default());

    let error = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("invalid utf-8"));

    fs::remove_file(path).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn proc_files_bypass_the_reusable_index() {
    let buffers = FileBufferStore::default();
    let reader = ReadFileTool::new(buffers.clone());

    let first = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: "/proc/uptime".to_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let second = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: "/proc/uptime".to_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();

    assert_ne!(first.content, second.content);
    assert!(buffers.files.read().await.is_empty());
}

#[tokio::test]
async fn write_file_invalidates_the_shared_read_buffer() {
    let path = temporary_file("write-invalidation");
    fs::write(&path, "old").await.unwrap();
    let buffers = FileBufferStore::default();
    let reader = ReadFileTool::new(buffers.clone());
    let writer = WriteFileTool::new(buffers.clone());

    let before = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(before.content, "old");
    assert_eq!(buffers.files.read().await.len(), 1);

    TypedTool::call(
        &writer,
        WriteFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            content: "new".to_owned(),
            mode: WriteFileMode::Overwrite,
        },
    )
    .await
    .unwrap();
    assert!(buffers.files.read().await.is_empty());

    let after = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(after.content, "new");

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn write_file_appends_and_invalidates_the_shared_read_buffer() {
    let path = temporary_file("append-invalidation");
    fs::write(&path, "first\n").await.unwrap();
    let buffers = FileBufferStore::default();
    let reader = ReadFileTool::new(buffers.clone());
    let writer = WriteFileTool::new(buffers.clone());

    TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(buffers.files.read().await.len(), 1);

    TypedTool::call(
        &writer,
        WriteFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            content: "second\n".to_owned(),
            mode: WriteFileMode::Append,
        },
    )
    .await
    .unwrap();
    assert!(buffers.files.read().await.is_empty());

    let output = TypedTool::call(
        &reader,
        ReadFileToolArgs {
            path: path.to_string_lossy().into_owned(),
            start_line: None,
            end_line: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(output.content, "first\nsecond");

    fs::remove_file(path).await.unwrap();
}

#[test]
fn bash_arguments_deserialize_by_action() {
    let run_blocking: BashToolArgs = serde_json::from_value(json!({
        "action": "run_blocking",
        "command": "cargo test"
    }))
    .unwrap();
    assert!(matches!(
        run_blocking,
        BashToolArgs::RunBlocking { command } if command == "cargo test"
    ));

    let run_background: BashToolArgs = serde_json::from_value(json!({
        "action": "run_background",
        "command": "cargo watch"
    }))
    .unwrap();
    assert!(matches!(
        run_background,
        BashToolArgs::RunBackground {
            command,
            session_id: None,
        } if command == "cargo watch"
    ));

    let send: BashToolArgs = serde_json::from_value(json!({
        "action": "send",
        "command": null,
        "session_id": "session-1",
        "input": "yes\n"
    }))
    .unwrap();
    assert!(matches!(
        send,
        BashToolArgs::Send { session_id, input }
            if session_id == "session-1" && input == "yes\n"
    ));
}

#[test]
fn bash_arguments_reject_missing_action_specific_fields() {
    for arguments in [
        json!({"action": "run_blocking"}),
        json!({"action": "send", "session_id": "session-1"}),
        json!({"action": "wait"}),
    ] {
        assert!(serde_json::from_value::<BashToolArgs>(arguments).is_err());
    }
}

#[test]
fn bash_presenter_presents_running_command() {
    let call = call(
        "bash",
        json!({
            "action": "run_blocking",
            "command": "printf 'one\ntwo'",
        }),
    );

    let presentation = BashPresenter.running(&call);

    assert!(matches!(presentation.status, ToolCallStatus::Running));
    assert_eq!(presentation.name, "Bash");
    assert_eq!(presentation.label, "built-in");
    assert_eq!(presentation.target.as_deref(), Some("printf 'one\\ntwo'"));
    assert!(presentation.blocks.is_empty());
}

#[test]
fn bash_presenter_separates_blocking_output_streams() {
    let call = call(
        "bash",
        json!({
            "action": "run_blocking",
            "command": "cargo test",
        }),
    );
    let result = ToolCallResult::success(
        call.id.clone(),
        serde_json::to_value(BashToolOutput::RanBlocking {
            stdout: "\u{1b}[32mok\u{1b}[0m\n".to_owned(),
            stderr: "warning\n".to_owned(),
        })
        .unwrap(),
    );

    let presentation = BashPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert!(matches!(
        &presentation.blocks[..],
        [
            DisplayBlock::Summary(summary),
            DisplayBlock::Summary(stdout_label),
            DisplayBlock::CodeBlock {
                language: Some(language),
                content: stdout,
                truncated_lines: 1,
                show_line_numbers: false,
                start_line_number: 1,
            },
            DisplayBlock::Summary(stderr_label),
            DisplayBlock::CodeBlock {
                language: Some(stderr_language),
                content: stderr,
                truncated_lines: 1,
                show_line_numbers: false,
                start_line_number: 1,
            },
        ] if summary == "Command completed"
            && stdout_label == "stdout"
            && language == "console"
            && stdout == "ok"
            && stderr_label == "stderr"
            && stderr_language == "console"
            && stderr == "warning"
    ));
}

#[test]
fn bash_presenter_surfaces_background_session_id() {
    let call = call(
        "bash",
        json!({
            "action": "run_background",
            "command": "cargo watch",
        }),
    );
    let result = ToolCallResult::success(
        call.id.clone(),
        serde_json::to_value(BashToolOutput::Spawned {
            session_id: "session-1".to_owned(),
        })
        .unwrap(),
    );

    let presentation = BashPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert!(matches!(
        &presentation.blocks[..],
        [
            DisplayBlock::Summary(summary),
            DisplayBlock::KeyValue { entries },
        ] if summary == "Started background session"
            && entries.len() == 1
            && entries[0].key == "session_id"
            && entries[0].value == "session-1"
    ));
}

#[test]
fn bash_presenter_does_not_echo_sent_input() {
    let call = call(
        "bash",
        json!({
            "action": "send",
            "session_id": "session-1",
            "input": "super-secret-password\n",
        }),
    );

    let presentation = BashPresenter.running(&call);

    assert_eq!(presentation.target.as_deref(), Some("session-1"));
    assert!(matches!(
        &presentation.blocks[..],
        [DisplayBlock::Summary(summary)] if summary == "Sending input"
    ));
    assert!(!format!("{:?}", presentation.blocks).contains("super-secret-password"));
}

#[test]
fn bash_presenter_marks_session_state_errors_as_failed() {
    let call = call(
        "bash",
        json!({
            "action": "view",
            "session_id": "session-1",
        }),
    );

    for (output, expected) in [
        (
            BashToolOutput::NoBusyCommand,
            "No command is running in this session",
        ),
        (BashToolOutput::SessionBusy, "Session is busy"),
        (BashToolOutput::SessionNotExist, "Session does not exist"),
    ] {
        let result =
            ToolCallResult::success(call.id.clone(), serde_json::to_value(output).unwrap());

        let presentation = BashPresenter.completed(&call, &result);

        assert!(matches!(
            presentation.status,
            ToolCallStatus::Failed { ref message } if message == expected
        ));
        assert!(matches!(
            &presentation.blocks[..],
            [DisplayBlock::Summary(summary)] if summary == expected
        ));
    }
}

#[test]
fn bash_presenter_presents_waited_session_output() {
    let call = call(
        "bash",
        json!({
            "action": "wait",
            "session_id": "session-1",
        }),
    );
    let result = ToolCallResult::success(
        call.id.clone(),
        serde_json::to_value(BashToolOutput::Output {
            output: "\u{1b}[32mdone\u{1b}[0m\r\n".to_owned(),
        })
        .unwrap(),
    );

    let presentation = BashPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(presentation.target.as_deref(), Some("session-1"));
    assert!(matches!(
        &presentation.blocks[..],
        [
            DisplayBlock::Summary(summary),
            DisplayBlock::CodeBlock {
                content,
                truncated_lines: 1,
                show_line_numbers: false,
                ..
            },
        ] if summary == "Session finished" && content == "done"
    ));
}

#[test]
fn write_file_mode_defaults_to_overwrite() {
    let arguments: WriteFileToolArgs = serde_json::from_value(json!({
        "path": "example.txt",
        "content": "replacement",
    }))
    .unwrap();

    assert!(matches!(arguments.mode, WriteFileMode::Overwrite));
}

#[test]
fn write_file_presenter_describes_append_mode() {
    let call = call(
        "write_file",
        json!({
            "path": "example.txt",
            "content": "one\ntwo",
            "mode": "append",
        }),
    );
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({ "status": "Ok" })),
    };
    let presentation = WriteFilePresenter.completed(&call, &result);

    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Appended 2 lines"
    ));
}

#[test]
fn humanizes_tool_names() {
    assert_eq!(humanize_tool_name("read_file"), "Read File");
    assert_eq!(humanize_tool_name("web-search"), "Web Search");
    assert_eq!(humanize_tool_name("GitHub_API"), "GitHub API");
    assert_eq!(humanize_tool_name("___"), "Tool");
}

#[test]
fn running_presents_sorted_redacted_arguments() {
    let presentation = DefaultPresenter.running(&call(
        "custom_tool",
        json!({
            "zeta": 42,
            "api_key": "secret-value",
            "alpha": true,
        }),
    ));

    assert!(matches!(presentation.status, ToolCallStatus::Running));
    assert_eq!(presentation.name, "Custom Tool");
    assert_eq!(presentation.label, "tool");
    assert!(presentation.target.is_none());

    let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
        panic!("expected key-value arguments");
    };
    assert_eq!(entries[0].key, "alpha");
    assert_eq!(entries[1].key, "api_key");
    assert_eq!(entries[1].value, REDACTED);
    assert_eq!(entries[2].key, "zeta");
}

#[test]
fn running_presents_non_object_arguments_as_text() {
    for arguments in [json!("hello"), json!([1, 2, 3]), json!(true)] {
        let presentation = DefaultPresenter.running(&call("tool", arguments));
        assert!(matches!(
            presentation.blocks[0],
            DisplayBlock::TextOutput { .. }
        ));
    }
}

#[test]
fn completed_presents_successful_object_output() {
    let call = call("lookup", json!({}));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "status": "ok",
            "token": "must-not-leak",
        })),
    };
    let presentation = DefaultPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
        panic!("expected key-value output");
    };
    assert_eq!(entries[0].key, "status");
    assert_eq!(entries[1].key, "token");
    assert_eq!(entries[1].value, REDACTED);
}

#[test]
fn recursively_redacts_nested_sensitive_fields() {
    let presentation = DefaultPresenter.running(&call(
        "nested",
        json!({
            "config": {
                "authorization": "Bearer secret",
                "nested": [{ "password": "secret" }],
            }
        }),
    ));

    let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
        panic!("expected key-value arguments");
    };
    assert!(entries[0].value.contains(REDACTED));
    assert!(!entries[0].value.contains("Bearer secret"));
    assert!(!entries[0].value.contains("\"secret\""));
}

#[test]
fn completed_presents_failure_with_truncated_message() {
    let call = call("failing_tool", json!({}));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Failure {
            message: "错误".repeat(MAX_ERROR_CHARS),
        },
    };
    let presentation = DefaultPresenter.completed(&call, &result);

    let ToolCallStatus::Failed { message } = presentation.status else {
        panic!("expected failed status");
    };
    assert!(message.ends_with("… [truncated]"));
    assert!(matches!(presentation.blocks[0], DisplayBlock::Summary(_)));
}

#[test]
fn fetch_presenter_presents_successful_status() {
    let call = call(
        "fetch",
        json!({ "url": "https://example.com", "raw": false }),
    );
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({ "result": "Example" })),
    };
    let presentation = FetchPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(presentation.name, "Fetch");
    assert_eq!(presentation.label, "built-in");
    assert_eq!(presentation.target.as_deref(), Some("https://example.com"));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "200 OK"
    ));
}

#[test]
fn fetch_presenter_uses_failure_message_as_summary() {
    let call = call(
        "fetch",
        json!({ "url": "https://example.com/missing", "raw": false }),
    );
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Failure {
            message: "404 Not Found".to_owned(),
        },
    };
    let presentation = FetchPresenter.completed(&call, &result);

    assert!(matches!(
        presentation.status,
        ToolCallStatus::Failed { ref message } if message == "404 Not Found"
    ));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "404 Not Found"
    ));
}

#[test]
fn grep_presenter_presents_running_query() {
    let call = call(
        "grep",
        json!({
            "path": "src",
            "pattern": "parse_markdown",
            "before": 1,
            "after": 2,
        }),
    );

    let presentation = GrepPresenter.running(&call);

    assert!(matches!(presentation.status, ToolCallStatus::Running));
    assert_eq!(presentation.name, "Grep");
    assert_eq!(presentation.label, "built-in");
    assert_eq!(presentation.target.as_deref(), Some("src"));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Searching for \"parse_markdown\""
    ));
}

#[test]
fn grep_presenter_presents_matches() {
    let call = call(
        "grep",
        json!({
            "path": "src",
            "pattern": "fn main",
            "before": 0,
            "after": 0,
        }),
    );
    let results = "src/main.rs:21:fn main() {}\nsrc/bin.rs:9:fn main() {}\n";
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({ "results": results })),
    };

    let presentation = GrepPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(presentation.target.as_deref(), Some("src"));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Returned 2 lines"
    ));
    assert!(matches!(
        &presentation.blocks[1],
        DisplayBlock::TextOutput {
            content,
            truncated_lines: 0,
        } if content == results.trim_end()
    ));
}

#[test]
fn grep_presenter_presents_no_matches() {
    let call = call(
        "grep",
        json!({ "path": "src", "pattern": "missing", "before": 0, "after": 0 }),
    );
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({ "results": "\n" })),
    };

    let presentation = GrepPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(presentation.blocks.len(), 1);
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "No matches"
    ));
}

#[test]
fn grep_presenter_presents_failure() {
    let call = call(
        "grep",
        json!({ "path": "src", "pattern": "[", "before": 0, "after": 0 }),
    );
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Failure {
            message: "unclosed character class".to_owned(),
        },
    };

    let presentation = GrepPresenter.completed(&call, &result);

    assert!(matches!(
        presentation.status,
        ToolCallStatus::Failed { ref message } if message == "unclosed character class"
    ));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Grep failed"
    ));
}

#[test]
fn read_file_presenter_presents_successful_output() {
    let call = call("read_file", json!({ "path": "src/main.rs" }));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "content": "fn two() {}\nfn three() {}",
            "start_line": 2,
            "end_line": 3,
            "total_lines": 10,
            "has_more": true,
        })),
    };
    let presentation = ReadFilePresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(presentation.name, "ReadFile");
    assert_eq!(presentation.label, "built-in");
    assert_eq!(presentation.target.as_deref(), Some("src/main.rs"));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Read lines 2–3 of 10"
    ));
    assert!(matches!(
        &presentation.blocks[1],
        DisplayBlock::CodeBlock {
            language: Some(language),
            content,
            truncated_lines: 10,
            show_line_numbers: true,
            start_line_number: 2,
        } if language == "raw" && content == "fn two() {}\nfn three() {}"
    ));
}

#[test]
fn read_file_presenter_presents_unknown_total() {
    let call = call("read_file", json!({ "path": "large.rs" }));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "content": "line 1\nline 2",
            "start_line": 1,
            "end_line": 2,
            "total_lines": null,
            "has_more": true,
        })),
    };
    let presentation = ReadFilePresenter.completed(&call, &result);

    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary)
            if summary == "Read lines 1–2 (total unknown; more available)"
    ));
}

#[test]
fn read_file_presenter_omits_code_block_past_eof() {
    let call = call("read_file", json!({ "path": "small.rs" }));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "content": "",
            "start_line": 10,
            "end_line": null,
            "total_lines": 3,
            "has_more": false,
        })),
    };
    let presentation = ReadFilePresenter.completed(&call, &result);

    assert_eq!(presentation.blocks.len(), 1);
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary)
            if summary == "No lines at or after 10 (file has 3 lines)"
    ));
}

#[test]
fn read_file_presenter_presents_failure() {
    let call = call("read_file", json!({ "path": "missing.rs" }));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Failure {
            message: "not found".to_owned(),
        },
    };
    let presentation = ReadFilePresenter.completed(&call, &result);

    assert!(matches!(
        presentation.status,
        ToolCallStatus::Failed { ref message } if message == "not found"
    ));
    assert!(matches!(
        &presentation.blocks[0],
        DisplayBlock::Summary(summary) if summary == "Failed to read file"
    ));
}

#[test]
fn truncates_long_multiline_unicode_output_safely() {
    let content = (0..30)
        .map(|index| format!("第 {index} 行 {}", "界".repeat(300)))
        .collect::<Vec<_>>()
        .join("\n");
    let call = call("long_output", json!({}));
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(Value::String(content)),
    };
    let presentation = DefaultPresenter.completed(&call, &result);

    let DisplayBlock::TextOutput {
        content,
        truncated_lines,
    } = &presentation.blocks[0]
    else {
        panic!("expected text output");
    };
    assert!(content.ends_with("… [truncated]"));
    assert!(*truncated_lines > 0);
    assert!(content.is_char_boundary(content.len()));
}
