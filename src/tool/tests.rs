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

fn edit_call(source: &str, target: &str) -> ToolCall {
    call(
        "edit",
        json!({"path": "src/main.rs", "source": source, "target": target}),
    )
}

fn applied_result(
    call: &ToolCall,
    start_line: usize,
    before: &[&str],
    after: &[&str],
) -> ToolCallResult {
    ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "status": {"Ok": {
                "start_line": start_line,
                "context_before": before,
                "context_after": after,
            }},
            "applied": true,
        })),
    }
}

fn diff_of(presentation: &Presentation) -> Vec<DiffLine> {
    let DisplayBlock::Diff { lines } = &presentation.blocks[1] else {
        panic!("expected a diff block");
    };

    lines.clone()
}

/// `<number> <sign><text>`, the way the view lays a diff line out.
fn rendered(lines: &[DiffLine]) -> Vec<String> {
    let width = lines
        .iter()
        .map(|line| line.number)
        .max()
        .unwrap_or(0)
        .to_string()
        .len();

    lines
        .iter()
        .map(|line| {
            let sign = match line.kind {
                DiffLineKind::Removed => '-',
                DiffLineKind::Added => '+',
                DiffLineKind::Context => ' ',
            };

            format!("{:>width$} {sign}{}", line.number, line.text)
        })
        .collect()
}

#[test]
fn edit_presenter_tags_removed_added_and_context_lines() {
    let call = edit_call("old\n", "new\n");
    let presentation = EditPresenter.completed(&call, &applied_result(&call, 10, &[], &[]));

    assert_eq!(presentation.name, "Edit");
    assert_eq!(presentation.target.as_deref(), Some("src/main.rs"));
    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));

    let lines = diff_of(&presentation);

    assert_eq!(lines[0].kind, DiffLineKind::Removed);
    assert_eq!(lines[1].kind, DiffLineKind::Added);
    assert_eq!(rendered(&lines), ["10 -old", "10 +new"]);
}

/// A context line that begins with a sign must stay context; this is why the kind
/// travels with the line instead of being read back out of the text.
#[test]
fn edit_presenter_keeps_a_dashed_context_line_as_context() {
    let call = edit_call("old\n", "new\n");
    let presentation = EditPresenter.completed(&call, &applied_result(&call, 2, &["---"], &[]));

    let lines = diff_of(&presentation);

    assert_eq!(lines[0].kind, DiffLineKind::Context);
    assert_eq!(lines[0].text, "---");
}

#[test]
fn edit_presenter_frames_the_change_with_file_line_numbers() {
    let call = edit_call("ten\neleven\n", "TEN\nELEVEN\n");
    let presentation = EditPresenter.completed(
        &call,
        &applied_result(
            &call,
            10,
            &["seven", "eight", "nine"],
            &["twelve", "thirteen", "fourteen"],
        ),
    );

    assert_eq!(
        rendered(&diff_of(&presentation)),
        [
            " 7  seven",
            " 8  eight",
            " 9  nine",
            "10 -ten",
            "11 -eleven",
            "10 +TEN",
            "11 +ELEVEN",
            "12  twelve",
            "13  thirteen",
            "14  fourteen",
        ]
    );
}

/// Adding lines pushes the trailing context down, so it is numbered on the
/// post-edit side.
#[test]
fn edit_presenter_numbers_trailing_context_after_the_new_block() {
    let call = edit_call("ten\n", "ten\nextra\n");
    let presentation = EditPresenter.completed(&call, &applied_result(&call, 10, &[], &["eleven"]));

    let lines = diff_of(&presentation);
    let last = lines.last().unwrap();

    assert_eq!(last.kind, DiffLineKind::Context);
    assert_eq!(last.text, "eleven");
    assert_eq!(last.number, 12, "one line was inserted above it");
}

#[test]
fn edit_presenter_summarizes_only_changed_lines() {
    let call = edit_call("keep\na\nb\n", "keep\nc\n");
    let presentation = EditPresenter.completed(&call, &applied_result(&call, 1, &[], &[]));

    let DisplayBlock::Summary(summary) = &presentation.blocks[0] else {
        panic!("expected a summary");
    };

    assert_eq!(summary, "-2 +1 lines", "context lines are not counted");
}

#[test]
fn edit_presenter_keeps_every_line_of_a_large_diff() {
    let source = (0..500)
        .map(|index| format!("old {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let target = (0..500)
        .map(|index| format!("new {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = edit_call(&source, &target);
    let presentation = EditPresenter.completed(&call, &applied_result(&call, 1, &[], &[]));

    let lines = diff_of(&presentation);

    assert_eq!(lines.len(), 1000, "500 removed plus 500 added");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count(),
        500
    );
}

#[test]
fn edit_presenter_reports_an_unapplied_edit_as_a_failure() {
    let call = edit_call("missing\n", "replacement\n");
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "status": {"NoCandidate": {"message": "There is no candidate"}},
            "applied": false,
        })),
    };

    let presentation = EditPresenter.completed(&call, &result);

    assert!(
        matches!(
            &presentation.status,
            ToolCallStatus::Failed { message } if message == "There is no candidate"
        ),
        "a rejected edit must not read as a success: {:?}",
        presentation.status
    );
    assert_eq!(
        presentation.blocks.len(),
        1,
        "no diff for an edit that never happened"
    );
}

#[test]
fn edit_presenter_counts_the_candidates_of_an_ambiguous_edit() {
    let call = edit_call("dup\n", "unique\n");
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({
            "status": {"MultipleExactMatches": {"candidates": [
                {"start_line": 1, "end_line": 2},
                {"start_line": 9, "end_line": 10},
            ]}},
            "applied": false,
        })),
    };

    let presentation = EditPresenter.completed(&call, &result);

    assert!(
        matches!(
            &presentation.status,
            ToolCallStatus::Failed { message }
                if message == "Source matches 2 places exactly (lines 1, 9); make it unique"
        ),
        "{:?}",
        presentation.status
    );
}

#[test]
fn edit_presenter_names_a_missing_file() {
    let call = edit_call("a\n", "b\n");
    let result = ToolCallResult {
        id: call.id.clone(),
        outcome: ToolCallOutcome::Success(json!({"status": "FileNotFound", "applied": false})),
    };

    let presentation = EditPresenter.completed(&call, &result);

    assert!(matches!(
        &presentation.status,
        ToolCallStatus::Failed { message } if message == "File not found"
    ));
}

#[test]
fn context_stops_at_the_edges_of_the_file() {
    let content = "one\ntwo\nthree\n";

    let (before, after) = super::edit::surrounding_context(content, 1, 1);
    assert!(before.is_empty(), "nothing precedes the first line");
    assert_eq!(after, ["two", "three"]);

    let (before, after) = super::edit::surrounding_context(content, 3, 1);
    assert_eq!(before, ["one", "two"]);
    assert!(after.is_empty(), "nothing follows the last line");
}

#[test]
fn context_is_capped_at_three_lines_on_each_side() {
    let content = (1..=20)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");

    let (before, after) = super::edit::surrounding_context(&content, 10, 2);

    assert_eq!(before, ["line 7", "line 8", "line 9"]);
    assert_eq!(after, ["line 12", "line 13", "line 14"]);
}

/// Closes the contract between the tool and its presenter: the other presenter
/// tests hand-write the output JSON, so only this one proves the presenter reads
/// the shape the tool actually emits.
#[tokio::test]
async fn edit_tool_output_feeds_its_presenter_end_to_end() {
    let path = temporary_file("edit-end-to-end");
    let content = (1..=14)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{content}\n")).await.unwrap();

    let output = TypedTool::call(
        &EditTool,
        EditToolArgs {
            path: path.to_string_lossy().into_owned(),
            source: "line 10\nline 11\n".to_owned(),
            target: "TEN\nELEVEN\n".to_owned(),
        },
    )
    .await
    .unwrap();

    let call = call(
        "edit",
        json!({
            "path": path.to_string_lossy(),
            "source": "line 10\nline 11\n",
            "target": "TEN\nELEVEN\n",
        }),
    );
    let result = ToolCallResult::success(call.id.clone(), serde_json::to_value(&output).unwrap());
    let presentation = EditPresenter.completed(&call, &result);

    assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
    assert_eq!(
        rendered(&diff_of(&presentation)),
        [
            " 7  line 7",
            " 8  line 8",
            " 9  line 9",
            "10 -line 10",
            "11 -line 11",
            "10 +TEN",
            "11 +ELEVEN",
            "12  line 12",
            "13  line 13",
            "14  line 14",
        ]
    );

    fs::remove_file(path).await.unwrap();
}

#[test]
fn exact_matches_report_every_occurrence_with_its_line_range() {
    let content = "a\nDUP\nb\nc\nDUP\nd\n";

    let matches = super::edit::exact_matches(content, "DUP\n")
        .iter()
        .map(|hit| (hit.start_line(), hit.end_line()))
        .collect::<Vec<_>>();

    assert_eq!(matches, [(2, 2), (5, 5)]);
}

#[test]
fn exact_matches_span_the_lines_of_a_multiline_source() {
    let content = "one\ntwo\nthree\nfour\n";

    let matches = super::edit::exact_matches(content, "two\nthree\n")
        .iter()
        .map(|hit| (hit.start_line(), hit.end_line()))
        .collect::<Vec<_>>();

    assert_eq!(matches, [(2, 3)]);
}

#[test]
fn an_empty_source_matches_nothing() {
    assert!(super::edit::exact_matches("anything\n", "").is_empty());
}

#[tokio::test]
async fn edit_refuses_an_ambiguous_source_and_leaves_the_file_alone() {
    let path = temporary_file("edit-ambiguous");
    let content = "a\nDUP\nb\nc\nDUP\nd\n";
    fs::write(&path, content).await.unwrap();

    let output = TypedTool::call(
        &EditTool,
        EditToolArgs {
            path: path.to_string_lossy().into_owned(),
            source: "DUP\n".to_owned(),
            target: "CHANGED\n".to_owned(),
        },
    )
    .await
    .unwrap();

    let serialized = serde_json::to_value(&output).unwrap();

    assert_eq!(serialized["applied"], json!(false));
    assert_eq!(
        serialized["status"]["MultipleExactMatches"]["candidates"],
        json!([
            {"start_line": 2, "end_line": 2},
            {"start_line": 5, "end_line": 5},
        ]),
        "the caller needs the line numbers to widen its source"
    );
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        content,
        "an ambiguous edit must not touch the file"
    );

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn edit_replaces_only_the_range_that_matched() {
    let path = temporary_file("edit-single");
    fs::write(&path, "keep\nONCE\nkeep\n").await.unwrap();

    let output = TypedTool::call(
        &EditTool,
        EditToolArgs {
            path: path.to_string_lossy().into_owned(),
            source: "ONCE\n".to_owned(),
            target: "TWICE\n".to_owned(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&output).unwrap()["applied"],
        json!(true)
    );
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "keep\nTWICE\nkeep\n"
    );

    fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn edit_refuses_an_empty_source_rather_than_shredding_the_file() {
    let path = temporary_file("edit-empty-source");
    let content = "one\ntwo\n";
    fs::write(&path, content).await.unwrap();

    let output = TypedTool::call(
        &EditTool,
        EditToolArgs {
            path: path.to_string_lossy().into_owned(),
            source: String::new(),
            target: "INJECTED".to_owned(),
        },
    )
    .await
    .unwrap();

    let serialized = serde_json::to_value(&output).unwrap();

    assert_eq!(serialized["applied"], json!(false));
    assert_eq!(
        serialized["status"]["InvalidRange"]["message"],
        json!("source must not be empty")
    );
    assert_eq!(fs::read_to_string(&path).await.unwrap(), content);

    fs::remove_file(path).await.unwrap();
}
