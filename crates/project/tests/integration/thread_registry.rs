use agent_client_protocol::schema::v1 as acp;
use chrono::{DateTime, Utc};
use gpui::{AppContext as _, TestAppContext};
use project::{
    AgentId, ThreadId, ThreadLandingState, ThreadMetadata, ThreadRegistry, WorktreePaths,
};
use rpc::proto::{self, Message as _};
use std::path::PathBuf;
use util::path_list::PathList;

fn test_timestamp() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-02T03:04:05.123Z")
        .expect("valid timestamp")
        .with_timezone(&Utc)
}

fn test_metadata() -> ThreadMetadata {
    let folder_paths = PathList::new(&[
        PathBuf::from("/project/worktree-a"),
        PathBuf::from("/project/worktree-b"),
    ]);
    ThreadMetadata {
        thread_id: ThreadId::new(),
        session_id: Some(acp::SessionId::new("session-1")),
        agent_id: AgentId::new("test-agent"),
        title: Some("Generated title".into()),
        title_override: Some("User title".into()),
        parallel_attempt_group: Some("attempt-group".to_string()),
        landed: Some(ThreadLandingState::Merged),
        updated_at: test_timestamp(),
        created_at: Some(test_timestamp()),
        interacted_at: Some(test_timestamp()),
        worktree_paths: WorktreePaths::from_folder_paths(&folder_paths),
        remote_connection: None,
        archived: false,
        server_hosted: false,
    }
}

#[gpui::test]
fn test_thread_registry_upsert_list_remove(cx: &mut TestAppContext) {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let path = tempdir.path().join("agent_threads.json");
    let registry = cx.update(|cx| cx.new(|cx| ThreadRegistry::local(path, cx)));
    let metadata = test_metadata();
    let thread_id = metadata.thread_id;

    cx.update(|cx| {
        registry
            .update(cx, |registry, cx| registry.upsert(metadata.clone(), cx))
            .expect("upsert metadata");
    });

    let list = cx.update(|cx| registry.read(cx).list());
    assert_eq!(list, vec![metadata]);

    cx.update(|cx| {
        registry
            .update(cx, |registry, cx| registry.remove(thread_id, cx))
            .expect("remove metadata");
    });

    let list = cx.update(|cx| registry.read(cx).list());
    assert!(list.is_empty());
}

#[gpui::test]
fn test_thread_registry_persists_across_recreation(cx: &mut TestAppContext) {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let path = tempdir.path().join("agent_threads.json");
    let metadata = test_metadata();

    cx.update(|cx| {
        let registry = cx.new(|cx| ThreadRegistry::local(path.clone(), cx));
        registry
            .update(cx, |registry, cx| registry.upsert(metadata.clone(), cx))
            .expect("upsert metadata");
    });

    let list = cx.update(|cx| {
        let registry = cx.new(|cx| ThreadRegistry::local(path, cx));
        registry.read(cx).list()
    });

    assert_eq!(list, vec![metadata]);
}

#[gpui::test]
fn test_thread_registry_tolerates_unknown_landing_state(cx: &mut TestAppContext) {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let path = tempdir.path().join("agent_threads.json");
    let metadata = test_metadata();

    cx.update(|cx| {
        let registry = cx.new(|cx| ThreadRegistry::local(path.clone(), cx));
        registry
            .update(cx, |registry, cx| registry.upsert(metadata, cx))
            .expect("upsert metadata");
    });

    let content = std::fs::read_to_string(&path).expect("read persisted registry");
    std::fs::write(
        &path,
        content.replace("\"landed\": \"merged\"", "\"landed\": \"unknown-state\""),
    )
    .expect("write persisted registry");

    let list = cx.update(|cx| {
        let registry = cx.new(|cx| ThreadRegistry::local(path, cx));
        registry.read(cx).list()
    });

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].landed, None);
}

#[test]
fn test_agent_thread_metadata_proto_round_trip() {
    let metadata = test_metadata();
    let proto = metadata.to_proto().expect("metadata to proto");
    let bytes = proto.encode_to_vec();
    let decoded_proto =
        proto::AgentThreadMetadata::decode(bytes.as_slice()).expect("decode metadata proto");
    let decoded = ThreadMetadata::from_proto(decoded_proto).expect("metadata from proto");

    assert_eq!(decoded, metadata);
}

#[test]
fn test_agent_thread_metadata_proto_tolerates_unknown_landing_state() {
    let metadata = test_metadata();
    let mut proto = metadata.to_proto().expect("metadata to proto");
    proto.landed = Some("unknown-state".to_string());
    let decoded = ThreadMetadata::from_proto(proto).expect("metadata from proto");

    assert_eq!(decoded.landed, None);
}
