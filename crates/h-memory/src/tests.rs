use tempfile::TempDir;
use tokio::fs;

use h_core::{
    agent::Agent,
    config::ReasoningEffort,
    provider::openai::{OpenAIProvider, OpenAIProviderConfig},
};

use crate::tool::{
    ReadPresenter, ReadTool, SearchPresenter, SearchTool, WritePresenter, WriteTool,
};
use crate::{Draft, Scope, Store, scope::project_id};

async fn fixture() -> (TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).await.unwrap();

    let store = Store::open(temp.path().join("memory"), workspace)
        .await
        .unwrap();

    (temp, store)
}

fn draft(id: impl Into<String>, summary: impl Into<String>) -> Draft {
    let id = id.into();

    Draft {
        title: id.replace('-', " "),
        id,
        summary: summary.into(),
        keywords: vec!["rust".to_owned()],
        content: "# Notes\n\nPersistent detail.".to_owned(),
        expected_revision: None,
    }
}

#[test]
fn draft_normalizes_title_to_one_line() {
    let mut draft = draft("architecture", "Current crate boundaries.");
    draft.title = "  Project\n architecture  ".to_owned();

    let draft = draft.validate().unwrap();

    assert_eq!(draft.title, "Project architecture");
}

#[tokio::test]
async fn project_write_creates_a_topic_and_rebuilds_the_index() {
    let (temp, store) = fixture().await;

    let result = store
        .write(
            Scope::Project,
            draft("bash-execution", "Bash sessions and output behavior."),
        )
        .await
        .unwrap();

    assert!(result.created);
    assert_eq!(result.entry.id, "bash-execution");
    assert!(result.entry.path.is_file());

    let index = fs::read_to_string(
        temp.path()
            .join("memory/projects")
            .join(store.project_id())
            .join("INDEX.md"),
    )
    .await
    .unwrap();

    assert!(index.contains("[bash execution](topics/bash-execution.md)"));
    assert!(index.contains("Bash sessions and output behavior."));
}

#[tokio::test]
async fn updating_requires_the_revision_from_a_read() {
    let (_temp, store) = fixture().await;
    let created = store
        .write(
            Scope::Project,
            draft("architecture", "Current crate boundaries."),
        )
        .await
        .unwrap();

    let error = store
        .write(
            Scope::Project,
            draft("architecture", "Updated crate boundaries."),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read it first"));

    let mut update = draft("architecture", "Updated crate boundaries.");
    update.content = "# Architecture\n\nThe current boundaries.".to_owned();
    update.expected_revision = Some(created.entry.revision);

    let updated = store.write(Scope::Project, update).await.unwrap();

    assert!(!updated.created);
    assert_eq!(updated.entry.summary, "Updated crate boundaries.");
    assert!(updated.entry.content.contains("current boundaries"));
}

#[tokio::test]
async fn updating_rejects_a_stale_revision() {
    let (_temp, store) = fixture().await;
    let created = store
        .write(
            Scope::Project,
            draft("architecture", "Current crate boundaries."),
        )
        .await
        .unwrap();

    let mut first_update = draft("architecture", "First update.");
    first_update.expected_revision = Some(created.entry.revision.clone());
    store.write(Scope::Project, first_update).await.unwrap();

    let mut stale_update = draft("architecture", "Stale update.");
    stale_update.expected_revision = Some(created.entry.revision);
    let error = store.write(Scope::Project, stale_update).await.unwrap_err();

    assert!(error.to_string().contains("changed since it was read"));
}

#[tokio::test]
async fn search_defaults_to_user_and_current_project_memory() {
    let (_temp, store) = fixture().await;

    store
        .write(
            Scope::User,
            draft("commit-style", "User prefers concise commit messages."),
        )
        .await
        .unwrap();
    store
        .write(
            Scope::Project,
            draft("commit-hooks", "Project commit hooks require formatting."),
        )
        .await
        .unwrap();

    let combined = store.search("commit", None, 10).await.unwrap();
    let project = store
        .search("commit", Some(Scope::Project), 10)
        .await
        .unwrap();

    assert_eq!(combined.len(), 2);
    assert_eq!(project.len(), 1);
    assert_eq!(project[0].scope, Scope::Project);
}

#[tokio::test]
async fn omitted_snapshot_topics_remain_searchable() {
    let (_temp, store) = fixture().await;
    let summary = "x".repeat(170);

    for index in 0..120 {
        store
            .write(
                Scope::Project,
                draft(
                    format!("topic-{index:03}"),
                    format!("{summary} unique-{index:03}"),
                ),
            )
            .await
            .unwrap();
    }

    let snapshot = store.snapshot().await.unwrap();
    let hits = store.search("unique-119", None, 10).await.unwrap();

    assert!(snapshot.content.contains("Use `search_memory`"));
    assert!(snapshot.content.contains("Showing"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "topic-119");
}

#[tokio::test]
async fn reopening_rebuilds_a_missing_index_from_topics() {
    let (temp, store) = fixture().await;

    store
        .write(
            Scope::User,
            draft("workflow", "A reusable workflow preference."),
        )
        .await
        .unwrap();

    let index = temp.path().join("memory/user/INDEX.md");
    fs::remove_file(&index).await.unwrap();

    let workspace = temp.path().join("workspace");
    Store::open(temp.path().join("memory"), workspace)
        .await
        .unwrap();

    let rebuilt = fs::read_to_string(index).await.unwrap();
    assert!(rebuilt.contains("topics/workflow.md"));
}

#[tokio::test]
async fn subdirectories_of_the_same_repository_share_a_project_id() {
    let repo = gix::discover(".").unwrap();
    let worktree = repo.workdir().unwrap();
    let root = project_id(worktree).await.unwrap();
    let nested = project_id(&worktree.join("crates/h-core")).await.unwrap();

    assert_eq!(root, nested);
}

#[tokio::test]
async fn memory_ids_cannot_escape_the_topics_directory() {
    let (_temp, store) = fixture().await;
    let error = store.read(Scope::Project, "../outside").await.unwrap_err();

    assert!(error.to_string().contains("lowercase ASCII"));
}

#[tokio::test]
async fn memory_tools_initialize_with_the_openai_provider_schema() {
    let (_temp, store) = fixture().await;
    let provider = OpenAIProvider::from_config(OpenAIProviderConfig::new(
        "https://example.com/v1",
        "test-token",
        "test-model",
        ReasoningEffort::Medium,
    ));
    let mut agent = Agent::new(provider);

    agent
        .register_tool_with_presenter(ReadTool::new(store.clone()), ReadPresenter)
        .register_tool_with_presenter(SearchTool::new(store.clone()), SearchPresenter)
        .register_tool_with_presenter(WriteTool::new(store), WritePresenter);

    agent.initialize().unwrap();
}
